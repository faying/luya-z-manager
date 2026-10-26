# The doctor tests' fixture (crates/luyaz-core/src/doctor/tests.rs): a device with every command
# the doctor runs stubbed, under a fresh $T. `setup` exports the DOC_* overrides; a scenario then
# changes files under $T. Was the setup of touch-ui scripts/test/doctor/run.sh (gone with
# doctor-legacy.sh, Luya-Z rename F4).
# SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
setup() {
    T=$(mktemp -d); mkdir -p "$T/bin" "$T/initd" "$T/alerts" "$T/crash" "$T/uid" "$T/rcd" "$T/proc/100" "$T/proc/101"
    echo luyaz-ui >"$T/proc/100/comm"; echo luyaz-core >"$T/proc/101/comm"
    echo "1000.5 1" >"$T/uptime"; echo 995 >"$T/hb"
    echo "sync success" >"$T/sync"; echo 0 >"$T/mode"; echo 1 >"$T/poll"; echo 8 >"$T/aps"
    for s in luyaz-core luyaz-sentinel; do printf '#!/bin/sh\n' >"$T/initd/$s"; chmod +x "$T/initd/$s"; done
    { echo "#!/bin/sh"; for s in luyaz-core luyaz-sentinel; do echo "$T/initd/$s start"; done; echo "exit 0"; } >"$T/rc"
    cp "$T/rc" "$T/rc.orig" # the tests name its md5 (it holds $T)
    printf 'sync    ALL        zte_topsw_devui\nsync    ALL        zte_topsw_wlan\n' >"$T/daemon.conf"
    echo "+8612300000000" >"$T/alerts/sms-to"
    # anonymous RPC tightened: session access says false for every factory/ADB method
    for f in fac_open fac_close fac_reset fac_reboot fac_query_key adb_switch; do printf '{\n\t"access": false\n}\n' >"$T/acl-$f"; done
    cat >"$T/bin/ubus" <<X
#!/bin/sh
case "\$2 \$3" in
  "zwrt_topsw_daemon.sync get_sync_info") echo "{ \"noSyncModuleName\": \"\$(cat $T/sync)\" }" ;;
  "session access") cat $T/acl-\$(echo "\$4" | sed 's/.*"function":"\([^"]*\)".*/\1/') 2>/dev/null ;;
  "service list") [ -f $T/down-\$(echo "\$4" | sed 's/.*"name":"\([^"]*\)".*/\1/') ] && echo '{}' || echo '{ "x": { "instances": { "i": { "running": true } } } }' ;;
esac
X
    cat >"$T/bin/uci" <<X
#!/bin/sh
case "\$*" in
  *dm_update_mode*) cat $T/mode ;; *TURNOFFPOLLING*) cat $T/poll ;;
  "-q show zwrt_mqtt.config") cat $T/mqtt 2>/dev/null ;;
  "-q show zwrt_tr069.ManagementServer") cat $T/tr069 2>/dev/null ;;
  "-q show zwrt_zte_dadian_debug") cat $T/dadian 2>/dev/null ;;
esac
X
    # stock phone-home as on the device (10-04, B31): reporting on, TR-069 without a server
    printf "zwrt_mqtt.config=config\nzwrt_mqtt.config.reportFreqInS='1000'\nzwrt_mqtt.config.mqttOnreportEnable='1'\n" >"$T/mqtt"
    printf "zwrt_tr069.ManagementServer=zudata_mgt\nzwrt_tr069.ManagementServer.EnableCWMP='1'\nzwrt_tr069.ManagementServer.PeriodicInformInterval='3600'\n" >"$T/tr069"
    printf "zwrt_zte_dadian_debug.wlan.switch='1'\nzwrt_zte_dadian_debug.wlan.appid='1'\nzwrt_zte_dadian_debug.web.switch='1'\nzwrt_zte_dadian_debug.sms.switch='0'\n" >"$T/dadian"
    cat >"$T/bin/ps" <<X
#!/bin/sh
n=\$(cat $T/aps); i=0; while [ \$i -lt \$n ]; do echo "1 root 0 S /usr/sbin/hostapd -g x"; i=\$((i+1)); done
X
    printf '#!/bin/sh\ncase "$*" in *public/status*) cat %s/public 2>/dev/null ;; esac\nexit 0\n' "$T" >"$T/bin/wget"
    # fixed clock and disk, so the outputs can be compared with the goldens and snapshots
    printf '#!/bin/sh\ncase "$1" in +%%s) cat %s/now ;; *) echo "2026-09-27 10:00" ;; esac\n' "$T" >"$T/bin/date"
    printf '#!/bin/sh\necho "Filesystem 1K-blocks Used Available Use%% Mounted on"\necho "/dev/ubi0 4000000 2800000 1200000 70%% /data"\n' >"$T/bin/df"
    echo 1790500000 >"$T/now"
    echo 0123456789abcdef0123456789abcdef >"$T/devid"
    chmod +x "$T"/bin/*
    export DOC_UBUS=$T/bin/ubus DOC_UCI=$T/bin/uci DOC_PS=$T/bin/ps DOC_WGET=$T/bin/wget DOC_RC=$T/rc \
        DOC_INITD=$T/initd DOC_UPTIME=$T/uptime DOC_HEARTBEAT=$T/hb DOC_MARKER=$T/marker DOC_ALERTS=$T/alerts DOC_REGRESS_SMS_OFF=$T/regress-sms-off \
        DOC_CRASH=$T/crash DOC_UID_STATE=$T/uid DOC_OVERLAY_RCD=$T/rcd DOC_DAEMON_CONF=$T/daemon.conf DOC_DATA=/ DOC_PROC=$T/proc \
        DOC_DATE=$T/bin/date DOC_DF=$T/bin/df DOC_UID_WANT=$T/uid.want DOC_CLOCK_OK=$T/clock-ok DOC_WALL_LAST=$T/wall.last \
        DOC_FP=$T/fp DOC_FP_CHANGED=$T/fp-changed DOC_TS_TUNING=$T/tuning.env DOC_DEVICE_ID=$T/devid
    export DOC_CHILL_TEMPLATE=$T/tpl DOC_CHILL_ENV=$T/chill.env
}

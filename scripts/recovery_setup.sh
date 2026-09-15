#!/bin/sh
# Disable "subsystem crash -> integrated reboot" — switch to per-subsystem recovery (SSR)
# so future crashes can be diagnosed via coredumps + monitor log instead of full device reboot.

# Wait for the subsys interfaces to come up (after S98subsystem-ramdump.init)
i=0
while [ $i -lt 60 ]; do
  [ -e /sys/class/remoteproc/remoteproc0/recovery ] && [ -e /sys/kernel/cnss/recovery ] && break
  sleep 1
  i=$((i+1))
done

# Modem (MSS): recover the subsystem instead of escalating to full reboot, and dump on crash
echo enabled > /sys/class/remoteproc/remoteproc0/recovery 2>/dev/null
echo enabled > /sys/class/remoteproc/remoteproc0/coredump 2>/dev/null

# WLAN (WCN7851 via cnss): bit0 fw recovery + bit1 pcss recovery
echo 3 > /sys/kernel/cnss/recovery 2>/dev/null

date "+%Y-%m-%dT%H:%M:%S recovery_setup: mss_recovery=$(cat /sys/class/remoteproc/remoteproc0/recovery) mss_coredump=$(cat /sys/class/remoteproc/remoteproc0/coredump) cnss=$(cat /sys/kernel/cnss/recovery 2>/dev/null | tail -2 | tr '\n' ' ')" >> /data/log/monitor.log

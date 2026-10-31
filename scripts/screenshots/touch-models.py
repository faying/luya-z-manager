#!/usr/bin/env python3
"""Touch UI screenshot models (MODELS for touch.sh), made from one view model the render test dumped.

    python3 scripts/screenshots/touch-models.py SRC_MODEL.json OUT_DIR

SRC_MODEL is a whole view model (`luyaz-ui --render … --dump` / the C test's --dump-model); its
texts are made up again here: one made-up operator ("Example Mobile", 001/01), RFC 5737 addresses,
+1555 01xx numbers, English for every line the core writes, no Chinese left. CHILL is marked not
installed (`chill_absent`), so no CHILL line shows anywhere. Each scene gets a run log
(index-<scene>-<theme>-en.txt) that feeds a series of models before the shots, a new one (and
core_version) every chart step, so the 5-minute charts and Home's speed wave are full.

Scenes: us (Home), good (tabs, alerts, the charts), lowbat, op-notice (light, dark), bandlock.
"""
# SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
import copy
import json
import math
import os
import re
import sys

src, out = sys.argv[1], sys.argv[2]
os.makedirs(out, exist_ok=True)
base = json.load(open(src))

MAP = {
    '顺畅': 'Smooth', '2 条载波聚合 · 带宽充足': '2 carriers · plenty of bandwidth', '强': 'Strong', '小': 'Low',
    '正常': 'Normal', '无': 'None', '本地': 'Local', '激活 2/3': 'Active 2/3', '自动': 'Auto', '就绪': 'Ready',
    '完成': 'Done', '在家': 'Home', '外出': 'Out', '国外': 'Abroad',
    'Wi-Fi 看门狗重新打开了 Wi-Fi': 'Wi-Fi watchdog turned Wi-Fi back on',
    '触屏界面闪退，已自动重新打开': 'Screen UI crashed and was restarted', '短信告警': 'Alert SMS',
    '没配置号码：后台挂了你不会知道': 'No number set', '中国 广东 深圳': 'Exampleland', '电信': 'Example Mobile',
    '中国电信': 'Example Mobile', '中国联通': 'Other Mobile', '中国移动': 'Third Mobile', '中国': 'Exampleland',
    '原厂扫描会断网且拿不到数据，已停用': 'Off', '约可用 14 小时 51 分': 'About 14 h 51 min left',
    '中国联通 · 主号': 'Example Mobile · Main', 'China Telecom': 'Example Mobile', 'China Telecom 4G': 'Example Mobile 4G',
    'Chunghwa Telecom': 'Travel eSIM', 'CTNET': 'Example', 'ctnet': 'internet',
}


def fix(o):
    """Made-up texts: the map above, documentation addresses, test ICCIDs; any other Chinese goes."""
    if isinstance(o, dict):
        return {k: fix(v) for k, v in o.items()}
    if isinstance(o, list):
        return [fix(v) for v in o]
    if isinstance(o, str):
        if o in MAP:
            return MAP[o]
        o = re.sub(r'192\.168\.0\.(\d+)', r'192.0.2.\1', o).replace('192.168.0.0/24', '192.0.2.0/24')
        o = re.sub(r'\+?86\d{11}|\+886\d{9}', '+15550100', o)
        o = o.replace('8986 ', '8900 ').replace('8988 ', '8900 ').replace('8944 ', '8900 ')
        if re.search(r'[一-鿿]', o):
            return ''
    return o


d = copy.deepcopy(base)
# CHILL not installed: the model says so, and its own fields are empty
for k in list(d):
    if k.startswith('chill_'):
        v = d[k]
        d[k] = 0 if isinstance(v, int) else ('' if isinstance(v, str) else ([] if isinstance(v, list) else v))
if isinstance(d.get('chill_home'), dict):
    d['chill_home']['known'] = 0
d['chill_absent'] = 1
d['scenario']['chill_on'] = 0
d['netinfo']['proxy'] = {'present': 0, 'ip': '', 'geo': '', 'isp': '', 'node': '', 'err': ''}
d['netinfo']['chill_running'] = 0
d['netinfo']['scenes'] = []
d['esim_enabled_iccid'] = '89000000000000000001'
d['st_server'] = 'Example ISP (Exampleville)'
d['st_servers'] = [{'id': 1, 'name': 'Exampleville', 'sponsor': 'Example ISP', 'country': 'Exampleland'}]
now = d.get('now', 0)
for blk in ('data', 'live'):
    b = d[blk]
    b['mcc'] = 1
    b['mnc'] = 1
    b['inbox'][0].update(num='Example Mobile', text="You have used 80% of this month's data. 20.4 GB left.")
    b['inbox'][1].update(num='+15550123', text="See you at three tomorrow, don't forget the book.")
    b['inbox'][2].update(num='55501', text='Your code is 482913. It expires in 5 minutes.')
    # 事故 (incidents): one closed in the last day, the rest quiet
    b['regress'] = {
        'fresh': 1, 'has': 1, 'at': now, 'open_total': 0, 'open': [],
        'closed': [{'id': 'r1', 'kind': 'signal', 'name': 'Weak signal', 'sub': 'RSRP below -110 dBm',
                    'from': now - 3 * 3600, 'to': now - 3 * 3600 + 420, 'end': 'recovered', 'count': 1,
                    'observed': ''}],
    }
for p in ('home', 'serving'):
    d['netinfo'][p].update(mcc='001', mnc='01')
for i, o in enumerate(d['netinfo']['ops']):
    o['plmn'] = '0010%d' % (i + 1)
d = fix(d)


def scene(name):
    m = copy.deepcopy(d)
    if name == 'lowbat':
        for b in ('data', 'live'):
            m[b].update(batt_pct=8, batt_cur_ua=-420000)
        m['battery_est'] = 'About 40 min left'
    if name == 'op-notice':
        # the one-time 「自动退回已开」 card in the bottom row (DD18), from the core's op view
        m['screen_op'] = '{"rollback_enabled":true,"active":null,"last":null,"notice":"rollback_on"}'
    if name == 'bandlock':
        for b in ('data', 'live'):
            m[b].update(lte_bands='1,3,7,20,28', sa_bands='78', nsa_bands='78')
    return m


# a series before the shots: a chart point takes CHART_STEP (5) refreshes, a second each, and a new
# core_version; 5 minutes of points fill the charts
STEP, N = 5, 62
PAGES = [('to_tab 0', 'home', 'scr/0/0'), ('to_tab 1', 'cellular', 'scr/0/1'), ('to_tab 2', 'wifi', 'scr/0/2'),
         ('to_tab 4', 'system', 'scr/0/4')]
SUBS = [('SUB_ALERTS', 'alerts', 'scr/1/12'), ('SUB_LOCK', 'lock', 'scr/1/3')]
for name, themes in (('us', ['light']), ('good', ['light']), ('lowbat', ['light']),
                     ('op-notice', ['light', 'dark']), ('bandlock', ['light'])):
    last = scene(name)
    base_v = last.get('core_version', 1)
    lines = ['create']
    for i in range(N):
        m = copy.deepcopy(last)
        if i < N - 1:  # the last one is the scene as it is
            w = 0.5 + 0.5 * math.sin(i / 4.0) * math.cos(i / 11.0)
            for b in ('data', 'live'):
                m[b]['down_bps'] = int(last[b].get('down_bps', 0) * (0.35 + 1.3 * w))
                m[b]['up_bps'] = int(last[b].get('up_bps', 0) * (0.5 + w))
                m[b]['cpu_pct'] = 8 + int(14 * w)
                m[b]['mem_pct'] = 72 + (i % 5) // 2
        m['core_version'] = base_v + i
        h = '%s-%02d' % (name, i)
        json.dump(m, open(os.path.join(out, h + '.json'), 'w'), ensure_ascii=False)
        lines.append('refresh %d 0 %s' % (i * STEP + 1, h))
    lines.append('settle %d' % ((N + 2) * STEP * 1000))
    # a second on each page: the Wi-Fi switches are read once the tab is up
    for tab, page, path in PAGES:
        lines += [tab, 'settle 1500', 'shoot_page %s %s' % (page, path), 'page_done %s' % page]
    for sub, page, path in SUBS:
        lines += ['sub_open ' + sub, 'settle 1500', 'shoot_page %s %s' % (page, path), 'page_done %s' % page,
                  'sub_close', 'settle 200']
    for th in themes:
        open(os.path.join(out, 'index-%s-%s-en.txt' % (name, th)), 'w').write('\n'.join(lines) + '\n')
print(out)

#!/usr/bin/env python3
"""Turn `busctl monitor --json=short org.ofono` output into a replay file.

Keeps what oFono itself says: Manager ModemAdded/ModemRemoved, PropertyChanged
signals, and GetModems/GetProperties replies (interfaces don't announce their
initial properties, so replies become snapshots). Identifiers are scrubbed.

    dev/trace2replay.py trace.json > tests/traces/name.json
"""
import json
import re
import sys

# key -> replacement; IMSI/ICCID keep the MCC/MNC or issuer prefix the logic needs.
SCRUB = {
    "Serial": lambda v: "350000000000017",
    "SubscriberIdentity": lambda v: v[:5] + "0" * (len(v) - 5),
    "CardIdentifier": lambda v: v[:7] + "0" * (len(v) - 7),
    "SubscriberNumbers": lambda v: ["+10000000000" for _ in v],
    "ServiceCenterAddress": lambda v: "+10000000000",
    "NetworkAccessIdentity": lambda v: re.sub(r"\d{6,}", lambda m: m[0][:5] + "0" * (len(m[0]) - 5), v),
    "CellId": lambda v: 1,
    "LocationAreaCode": lambda v: 1,
}


def scrub(props):
    for key, val in props.items():
        if key in SCRUB:
            val["data"] = SCRUB[key](val["data"])
    return props


def main():
    msgs = [json.loads(l) for l in open(sys.argv[1]) if l.strip()]
    calls = {m["cookie"]: m for m in msgs if m["type"] == "method_call"}
    ofono = {m["sender"] for m in msgs
             if m["type"] == "signal" and (m.get("interface") or "").startswith("org.ofono.")}
    t0 = msgs[0]["timestamp-realtime"]
    events = []

    for m in msgs:
        at = (m["timestamp-realtime"] - t0) // 1000
        if m["type"] == "signal" and m["sender"] in ofono:
            iface, member, data = m["interface"], m["member"], m["payload"]["data"]
            if (iface, member) == ("org.ofono.Manager", "ModemAdded"):
                events.append({"at": at, "added": data[0], "props": scrub(data[1])})
            elif (iface, member) == ("org.ofono.Manager", "ModemRemoved"):
                events.append({"at": at, "removed": data[0]})
            elif member == "PropertyChanged" and iface.startswith("org.ofono."):
                key, val = data
                scrub({key: val})
                events.append({"at": at, "path": m["path"], "iface": iface, "changed": [key, val]})
        elif m["type"] == "method_return" and m["sender"] in ofono:
            call = calls.get(m.get("reply_cookie"))
            if not call or not m["payload"]["data"]:
                continue
            if call["member"] == "GetModems":
                for path, props in m["payload"]["data"][0]:
                    events.append({"at": at, "path": path, "iface": "org.ofono.Modem",
                                   "snapshot": scrub(props)})
            elif call["member"] == "GetProperties" and call["interface"].startswith("org.ofono."):
                events.append({"at": at, "path": call["path"], "iface": call["interface"],
                               "snapshot": scrub(m["payload"]["data"][0])})

    out = json.dumps(events, ensure_ascii=False, indent=1)
    # Any long digit run anywhere (not only whole strings: NAI is "<imsi>@...")
    # must be one of our placeholders.
    leaks = [n for n in re.findall(r"\d{9,}", out)
             if n not in {"350000000000017", "10000000000"} and not re.fullmatch(r"\d{5,7}0{8,}", n)]
    if leaks:
        sys.exit(f"unscrubbed identifiers: {leaks}")
    print(out)


main()

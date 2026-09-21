#!/usr/bin/env python3
"""A stand-in tray for the dev bus: owns org.kde.StatusNotifierWatcher, says a
host is there, and prints each item that registers. `scripts/dev tray` starts
it; the item's methods are then called with gdbus, as a tray click would."""

import sys

from gi.repository import Gio, GLib  # pyright: ignore[reportAttributeAccessIssue]

XML = """
<node>
  <interface name="org.kde.StatusNotifierWatcher">
    <method name="RegisterStatusNotifierItem"><arg type="s" direction="in"/></method>
    <method name="RegisterStatusNotifierHost"><arg type="s" direction="in"/></method>
    <property name="RegisteredStatusNotifierItems" type="as" access="read"/>
    <property name="IsStatusNotifierHostRegistered" type="b" access="read"/>
    <property name="ProtocolVersion" type="i" access="read"/>
    <signal name="StatusNotifierItemRegistered"><arg type="s"/></signal>
    <signal name="StatusNotifierHostRegistered"/>
  </interface>
</node>
"""

items: list[str] = []


def call(conn, sender, path, iface, method, params, invocation):
    if method == "RegisterStatusNotifierItem":
        (service,) = params.unpack()
        item = service if "/" in service else f"{service}/StatusNotifierItem"
        if item not in items:
            items.append(item)
        print(f"registered {item}", flush=True)
        conn.emit_signal(
            None,
            "/StatusNotifierWatcher",
            "org.kde.StatusNotifierWatcher",
            "StatusNotifierItemRegistered",
            GLib.Variant("(s)", (item,)),
        )
    invocation.return_value(None)


def get(conn, sender, path, iface, prop):
    return {
        "RegisteredStatusNotifierItems": GLib.Variant("as", items),
        "IsStatusNotifierHostRegistered": GLib.Variant("b", True),
        "ProtocolVersion": GLib.Variant("i", 0),
    }[prop]


def acquired(conn, name):
    info = Gio.DBusNodeInfo.new_for_xml(XML).interfaces[0]
    register = getattr(conn, "register_object_with_closures2", conn.register_object)
    register("/StatusNotifierWatcher", info, call, get, None)
    print("watching", flush=True)


def lost(conn, name):
    print("lost org.kde.StatusNotifierWatcher", flush=True)
    sys.exit(1)


Gio.bus_own_name(
    Gio.BusType.SESSION,
    "org.kde.StatusNotifierWatcher",
    Gio.BusNameOwnerFlags.NONE,
    None,
    acquired,
    lost,
)
GLib.MainLoop().run()

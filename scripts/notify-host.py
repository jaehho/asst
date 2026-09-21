#!/usr/bin/env python3
"""A stand-in notification server for the dev bus: owns
org.freedesktop.Notifications, prints each notification as one JSON line, and
has one method of its own, dev.jaeho.Test.Click(id, action), which does what a
click on a notification or one of its buttons does: an ActivationToken, then
ActionInvoked. `scripts/dev up` starts it before the dev asstd."""

import json
import sys

from gi.repository import Gio, GLib  # pyright: ignore[reportAttributeAccessIssue]

XML = """
<node>
  <interface name="org.freedesktop.Notifications">
    <method name="Notify">
      <arg type="s" direction="in"/><arg type="u" direction="in"/>
      <arg type="s" direction="in"/><arg type="s" direction="in"/>
      <arg type="s" direction="in"/><arg type="as" direction="in"/>
      <arg type="a{sv}" direction="in"/><arg type="i" direction="in"/>
      <arg type="u" direction="out"/>
    </method>
    <method name="CloseNotification"><arg type="u" direction="in"/></method>
    <method name="GetCapabilities"><arg type="as" direction="out"/></method>
    <method name="GetServerInformation">
      <arg type="s" direction="out"/><arg type="s" direction="out"/>
      <arg type="s" direction="out"/><arg type="s" direction="out"/>
    </method>
    <signal name="ActionInvoked"><arg type="u"/><arg type="s"/></signal>
    <signal name="ActivationToken"><arg type="u"/><arg type="s"/></signal>
    <signal name="NotificationClosed"><arg type="u"/><arg type="u"/></signal>
  </interface>
  <interface name="dev.jaeho.Test">
    <method name="Click"><arg type="u" direction="in"/><arg type="s" direction="in"/></method>
  </interface>
</node>
"""

PATH = "/org/freedesktop/Notifications"
IFACE = "org.freedesktop.Notifications"
last_id = 0


def call(conn, sender, path, iface, method, params, invocation):
    global last_id
    if method == "Notify":
        app, replaces, _icon, summary, body, actions, _hints, _timeout = params.unpack()
        last_id = replaces or last_id + 1
        pairs = dict(zip(actions[::2], actions[1::2]))
        line = {"id": last_id, "app": app, "summary": summary, "body": body, "actions": pairs}
        print(json.dumps(line), flush=True)
        invocation.return_value(GLib.Variant("(u)", (last_id,)))
    elif method == "CloseNotification":
        (nid,) = params.unpack()
        print(json.dumps({"closed": nid}), flush=True)
        conn.emit_signal(None, PATH, IFACE, "NotificationClosed", GLib.Variant("(uu)", (nid, 3)))
        invocation.return_value(None)
    elif method == "GetCapabilities":
        invocation.return_value(GLib.Variant("(as)", (["actions", "body"],)))
    elif method == "GetServerInformation":
        invocation.return_value(GLib.Variant("(ssss)", ("notify-host", "asst", "0", "1.2")))
    elif method == "Click":
        nid, action = params.unpack()
        conn.emit_signal(None, PATH, IFACE, "ActivationToken", GLib.Variant("(us)", (nid, "")))
        conn.emit_signal(None, PATH, IFACE, "ActionInvoked", GLib.Variant("(us)", (nid, action)))
        print(json.dumps({"clicked": nid, "action": action}), flush=True)
        invocation.return_value(None)


def acquired(conn, name):
    register = getattr(conn, "register_object_with_closures2", conn.register_object)
    for info in Gio.DBusNodeInfo.new_for_xml(XML).interfaces:
        register(PATH, info, call, None, None)
    print(json.dumps({"serving": name}), flush=True)


def lost(conn, name):
    print(json.dumps({"lost": name}), flush=True)
    sys.exit(1)


Gio.bus_own_name(
    Gio.BusType.SESSION,
    "org.freedesktop.Notifications",
    Gio.BusNameOwnerFlags.NONE,
    None,
    acquired,
    lost,
)
GLib.MainLoop().run()

#!/usr/bin/env python3
"""A stand-in steno for the dev bus: owns dev.jaeho.Steno, answers
org.freedesktop.Application on /dev/jaeho/Steno as steno's GApplication
does, and prints each call as one JSON line, so a check can read what
Open meeting asked for (the action, its argument, the activation token).
`scripts/dev steno` starts it, as the dev window's `steno`."""

import json
import sys

from gi.repository import Gio, GLib  # pyright: ignore[reportAttributeAccessIssue]

XML = """
<node>
  <interface name="org.freedesktop.Application">
    <method name="Activate"><arg type="a{sv}" direction="in"/></method>
    <method name="Open">
      <arg type="as" direction="in"/><arg type="a{sv}" direction="in"/>
    </method>
    <method name="ActivateAction">
      <arg type="s" direction="in"/><arg type="av" direction="in"/>
      <arg type="a{sv}" direction="in"/>
    </method>
  </interface>
</node>
"""

NAME = "dev.jaeho.Steno"
PATH = "/dev/jaeho/Steno"


def call(_conn, _sender, _path, _iface, method, params, invocation):
    """D-Bus method-call callback; the signature is fixed by Gio."""
    if method == "ActivateAction":
        action, args, platform = params.unpack()
        line = {"action": action, "args": args, "platform": platform}
    elif method == "Open":
        uris, platform = params.unpack()
        line = {"open": uris, "platform": platform}
    else:
        (platform,) = params.unpack()
        line = {"activate": True, "platform": platform}
    print(json.dumps(line), flush=True)
    invocation.return_value(None)


def acquired(_conn, name):
    print(json.dumps({"serving": name, "argv": sys.argv[1:]}), flush=True)


def lost(_conn, name):
    print(json.dumps({"lost": name}), flush=True)
    sys.exit(1)


# The object before the name, as GApplication does: a caller that sees
# the name owned finds the object there.
bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
register = getattr(bus, "register_object_with_closures2", bus.register_object)
for info in Gio.DBusNodeInfo.new_for_xml(XML).interfaces:
    register(PATH, info, call, None, None)
Gio.bus_own_name_on_connection(bus, NAME, Gio.BusNameOwnerFlags.NONE, acquired, lost)
GLib.MainLoop().run()

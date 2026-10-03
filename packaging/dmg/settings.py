# dmgbuild settings for the disk image window (see scripts/package.sh).
# Defines: app (path to the .app), background (multi-resolution TIFF).
import os.path

app = defines["app"]  # noqa: F821 (provided by dmgbuild)
name = os.path.basename(app)

format = "UDZO"
compression_level = 9
filesystem = "HFS+"
files = [app]
symlinks = {"Applications": "/Applications"}
hide_extensions = [name]

# 660x400 content (plus the title bar) over background.png; icons sit under the
# drawing's dimension line.
background = defines["background"]  # noqa: F821
window_rect = ((200, 140), (660, 432))
default_view = "icon-view"
show_status_bar = False
show_tab_view = False
show_toolbar = False
show_pathbar = False
show_sidebar = False
arrange_by = None
icon_size = 128
text_size = 13
icon_locations = {name: (170, 187), "Applications": (490, 187)}

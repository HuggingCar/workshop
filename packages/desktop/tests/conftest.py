import os

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

from PySide6.QtCore import QSettings, QStandardPaths  # after the platform is chosen

# Any QSettings a test forgets to isolate lands under ~/.qttest, never in the user's config
# (IniFormat: on Windows the native format would be the registry, which test mode cannot redirect).
QStandardPaths.setTestModeEnabled(True)
QSettings.setDefaultFormat(QSettings.Format.IniFormat)

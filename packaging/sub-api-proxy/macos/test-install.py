"""Check menu-bar installation without starting a GUI process."""
import os
from pathlib import Path
import plistlib
import subprocess
import tempfile

installer = Path(__file__).with_name('install.sh').resolve()
with tempfile.TemporaryDirectory(prefix='sap-menu-install-') as temporary:
    root = Path(temporary)
    app = root / 'source.app'
    executable = app / 'Contents/MacOS/Sub-Api-Proxy'
    executable.parent.mkdir(parents=True)
    executable.write_text('#!/bin/sh\nexit 0\n')
    executable.chmod(0o755)
    home = root / 'home & spaces'
    home.mkdir()
    stubs = root / 'stubs'
    stubs.mkdir()
    launchctl = stubs / 'launchctl'
    launchctl.write_text('#!/bin/sh\nprintf "%s\\n" "$*" >> "$SAP_MENU_TEST_LOG"\n')
    launchctl.chmod(0o755)
    log = root / 'launchctl.log'
    env = dict(os.environ, HOME=str(home), SAP_MENU_TEST_LOG=str(log),
               PATH=str(stubs) + os.pathsep + os.environ['PATH'])
    for _ in range(2):
        subprocess.run(['sh', str(installer), str(app)], check=True, env=env,
                       capture_output=True, timeout=15)
    installed = home / 'Applications/Sub-Api-Proxy.app'
    assert (installed / 'Contents/MacOS/Sub-Api-Proxy').read_bytes() == executable.read_bytes()
    assert len(list(installed.parent.glob('Sub-Api-Proxy.app.backup-*'))) == 1
    plist = home / 'Library/LaunchAgents/io.sub-api-proxy.menubar.plist'
    value = plistlib.loads(plist.read_bytes())
    assert value['ProgramArguments'] == [str(installed / 'Contents/MacOS/Sub-Api-Proxy')]
    assert value['KeepAlive'] == {'SuccessfulExit': False}
    assert plist.stat().st_mode & 0o777 == 0o600
    assert log.read_text().count('bootstrap') == 2
print('PASS: menu-bar install, repeat install, backup, and login launch configuration')

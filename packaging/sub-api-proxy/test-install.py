#!/usr/bin/env python3
"""Exercise both installers in temporary homes with fake service managers."""
import os
from pathlib import Path
import plistlib
import subprocess
import tempfile

installer = Path(__file__).resolve().with_name("install.sh")
with tempfile.TemporaryDirectory(prefix="sap-install-test-") as temporary:
    root = Path(temporary)
    binaries = root / "binaries"
    binaries.mkdir()
    for name in ("sap", "sap-router", "cliproxy"):
        binary = binaries / name
        binary.write_text("#!/bin/sh\nexit 0\n")
        binary.chmod(0o755)
    stubs = root / "stubs"
    stubs.mkdir()
    for name, text in {
        "uname": '#!/bin/sh\nprintf "%s\\n" "$SAP_TEST_OS"\n',
        "launchctl": '#!/bin/sh\nprintf "%s\\n" "$*" >> "$SAP_TEST_LOG"\n',
        "systemctl": '#!/bin/sh\nprintf "%s\\n" "$*" >> "$SAP_TEST_LOG"\n',
    }.items():
        stub = stubs / name
        stub.write_text(text)
        stub.chmod(0o755)
    for platform in ("Linux", "Darwin"):
        home = root / (platform + " home & test")
        home.mkdir()
        log = root / (platform + ".log")
        env = dict(os.environ, HOME=str(home), SAP_TEST_OS=platform,
                   SAP_TEST_LOG=str(log), PATH=str(stubs) + os.pathsep + os.environ["PATH"])
        command = [str(installer), "--role", "both", "--bin-dir", str(binaries)]
        subprocess.run(command + ["--dry-run"], env=env, check=True, capture_output=True)
        assert list(home.iterdir()) == []
        assert not log.exists()
        config = home / ".config/sub-api-proxy"
        config.mkdir(parents=True)
        for name in ("config.yaml", "sap.yaml", "router.yaml"):
            (config / name).write_text("preserve me\n")
        for _ in range(2):
            subprocess.run(command, env=env, check=True, capture_output=True)
            for name in ("sap", "sap-router", "cliproxy"):
                assert (home / ".local/bin" / name).read_bytes() == (binaries / name).read_bytes()
            for name in ("config.yaml", "sap.yaml", "router.yaml"):
                assert (config / name).read_text() == "preserve me\n"
        if platform == "Darwin":
            for role in ("server", "router"):
                plist = home / "Library/LaunchAgents" / f"io.sub-api-proxy.{role}.plist"
                with plist.open("rb") as stream:
                    value = plistlib.load(stream)
                assert value["ProgramArguments"][0].startswith(str(home))
                assert "@HOME@" not in plist.read_text()
        else:
            assert (home / ".config/systemd/user/sap-server.service").exists()
            assert (home / ".config/systemd/user/sap-router.service").exists()
        assert log.exists()
print("PASS: Linux and macOS install twice, preserve configs, dry-run writes nothing")

"""Package the tested desktop build without developer credentials or signing keys."""
import os
from pathlib import Path
import plistlib
import shutil
import subprocess
import sys
import zipfile


def package(name):
    root = Path("dist") / name
    root.mkdir(parents=True, exist_ok=True)
    if name == "macos-arm64":
        bundle = root / "Litecord.app" / "Contents"
        binary = bundle / "MacOS" / "litecord"
        binary.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2("target/release/litecord", binary)
        binary.chmod(0o755)
        with (bundle / "Info.plist").open("wb") as f:
            plistlib.dump({
                "CFBundleExecutable": "litecord",
                "CFBundleIdentifier": "dev.litecord.account",
                "CFBundleName": "Litecord",
                "CFBundleDisplayName": "Litecord",
                "CFBundlePackageType": "APPL",
                "CFBundleShortVersionString": "0.1.0",
                "CFBundleVersion": os.environ.get("GITHUB_RUN_NUMBER", "1"),
                "LSMinimumSystemVersion": "11.0",
                "NSHighResolutionCapable": True,
            }, f)
        subprocess.run(["codesign", "--force", "--sign", "-", str(root / "Litecord.app")], check=True)
    elif name == "windows-x64":
        shutil.copy2("target/release/litecord.exe", root / "Litecord.exe")
    else:
        raise ValueError(f"Unsupported package: {name}")
    shutil.copy2("docs/PART_B_PLUS_ACCOUNT_WRITES.md", root / "Account guide.md")
    (root / "Read me.txt").write_text(
        "Open Litecord to sign in from Settings. No compiler is required.\n"
        "This experimental build uses an account-owner supplied session credential, not OAuth.\n"
        "Enter your credential only in the app's masked field; it is saved in the OS keyring.\n"
        "This build is not notarized or signed with a publisher certificate.\n"
        f"Source commit: {os.environ.get('GITHUB_SHA', 'local')}\n",
        encoding="utf-8",
    )
    archive = Path("dist") / f"litecord-account-{name}.zip"
    with zipfile.ZipFile(archive, "w", zipfile.ZIP_DEFLATED) as z:
        for path in sorted(root.rglob("*")):
            z.write(path, path.relative_to(root))
    print(archive)


if __name__ == "__main__":
    package(sys.argv[1])

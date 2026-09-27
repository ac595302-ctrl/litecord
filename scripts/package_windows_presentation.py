"""Package a browser-login,screenshots Windows release. Run from the repo root."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import struct
import subprocess
import tomllib
import zipfile


def machine(path):
    data = path.read_bytes()
    offset = struct.unpack_from('<I', data, 0x3C)[0]
    if data[:2] != b'MZ' or data[offset:offset + 4] != b'PE\0\0':
        raise ValueError(f'Not a Windows PE image: {path}')
    return struct.unpack_from('<H', data, offset + 4)[0]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    repo = Path.cwd()
    root = args.output.resolve()
    binary = repo / 'target/release/litecord.exe'
    if machine(binary) != 0x8664:
        raise ValueError('This package requires the x64 Windows build')
    cargo_home = Path(os.environ.get('CARGO_HOME', Path.home() / '.cargo'))
    lock = tomllib.loads((repo / 'Cargo.lock').read_text(encoding='utf-8'))
    version = next(p['version'] for p in lock['package'] if p['name'] == 'webview2-com-sys')
    loaders = list((cargo_home / 'registry/src').glob(f'*/webview2-com-sys-{version}/x64/WebView2Loader.dll'))
    if not loaders:
        raise FileNotFoundError('The pinned x64 WebView2Loader.dll is missing; build browser-login first')
    loader = loaders[0]
    if machine(loader) != machine(binary):
        raise ValueError('WebView2 loader architecture does not match the executable')
    def git(*argv):
        return subprocess.check_output(['git', '-c', f'safe.directory={repo.as_posix()}', *argv], text=True).strip()
    if git('status', '--porcelain'):
        raise RuntimeError('Commit the source before packaging so build metadata is reproducible')
    commit = git('rev-parse', 'HEAD')
    root.mkdir(parents=True, exist_ok=False)
    shutil.copy2(binary, root / 'Litecord.exe')
    shutil.copy2(loader, root / 'WebView2Loader.dll')
    shutil.copy2(repo / 'config/litecord.account.example.toml', root / 'account.toml')
    (root / 'demo.toml').write_text('[backend]\nkind = "demo"\ndemo_bot = true\n', encoding='utf-8')
    for name, config, mode, folder in [
        ('Start Presentation Demo.cmd', 'demo.toml', 'demo', 'presentation-demo'),
        ('Start Litecord Account.cmd', 'account.toml', 'user-session', 'account'),
    ]:
        command = (f'@echo off\ncd /d "%~dp0"\n'
                   f'"%~dp0Litecord.exe" --config "%~dp0{config}" --backend {mode} '
                   f'--db "%LOCALAPPDATA%\\Litecord\\{folder}\\litecord.db" gui\n')
        (root / name).write_text(command, encoding='utf-8')
    docs = root / 'docs'
    docs.mkdir()
    for name in ['HOSTED_DISCORD_LOGIN.md', 'WINDOWS_PRESENTATION.md']:
        shutil.copy2(repo / 'docs' / name, docs / name)
    shutil.copy2(repo / 'crates/litecord-ui/assets/fonts/Inter-LICENSE.txt', docs / 'Inter-LICENSE.txt')
    (root / 'START HERE.txt').write_text(
        'Extract this entire folder before running. Keep Litecord.exe and WebView2Loader.dll together.\n\n'
        'Start Presentation Demo.cmd: synthetic conversations and a synthetic bot; no Discord login needed.\n'
        'Start Litecord Account.cmd: separate real-account database, read-only configuration. Open Settings to sign in.\n'
        'The hosted login requires the installed Microsoft Edge WebView2 Runtime.\n'
        'Complete any verification yourself using Open Discord sign-in. Password login cannot complete CAPTCHA.\n\n'
        'Account access is unofficial, including after browser sign-in. Account termination is possible.\n'
        'This build cannot restore a suspended account or guarantee live Discord compatibility.\n'
        'Account writes are disabled by account.toml. The demo remains usable offline.\n'
        'No account credentials or cached conversations are included in this package.\n'
        'No Windows security setting needs to be disabled as an installation step.\n\n'
        f'Source branch: {git("branch", "--show-current")}\nSource commit: {commit}\n'
        'See docs/WINDOWS_PRESENTATION.md for checks and limitations.\n', encoding='utf-8')
    metadata = {
        'commit': commit, 'tree': git('rev-parse', 'HEAD^{tree}'),
        'branch': git('branch', '--show-current'),
        'features': ['browser-login', 'screenshots'], 'platform': 'windows-x64',
        'files_sha256': {p.name: hashlib.sha256(p.read_bytes()).hexdigest()
                         for p in [root / 'Litecord.exe', root / 'WebView2Loader.dll']},
    }
    (root / 'build-info.json').write_text(json.dumps(metadata, indent=2) + '\n', encoding='utf-8')
    subprocess.run(['git', '-c', f'safe.directory={repo.as_posix()}', 'archive', '--format=zip',
                    f'--output={root / "source.zip"}', 'HEAD'], check=True)
    archive = root.with_suffix('.zip')
    with zipfile.ZipFile(archive, 'w', zipfile.ZIP_DEFLATED) as output:
        for path in sorted(root.rglob('*')):
            if path.is_file():
                output.write(path, path.relative_to(root))
    with zipfile.ZipFile(archive) as output:
        if output.testzip() is not None:
            raise RuntimeError('Package integrity check failed')
    print(json.dumps({'folder': str(root), 'archive': str(archive), **metadata}, indent=2))


if __name__ == '__main__':
    main()

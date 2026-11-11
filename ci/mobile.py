#!/usr/bin/env python3
"""Package a real Rust/NDI smoke app and execute only declared hosted simulators."""
from __future__ import annotations
import argparse
import json
import os
from pathlib import Path
import plistlib
import re
import shutil
import subprocess
import sys
import time
import tomllib
import zipfile
from common import ROOT, digest, profiles, write_json
import cargo_artifacts

BUNDLE = 'video.ndi.grafton.smoke'


def call(args, *, capture=False, timeout=300, env=None):
    result = subprocess.run([str(x) for x in args], check=True, text=True,
        stdout=subprocess.PIPE if capture else None, timeout=timeout, env=env)
    return result.stdout.strip() if capture else None


def lock_identity(path):
    with path.open('rb') as f:
        data = tomllib.load(f)
    return {(p['name'], p['version'], p.get('source'), p.get('checksum')) for p in data['package']}


def check_app_resolution(original, generated):
    extras = {p for p in lock_identity(generated) - lock_identity(original) if p[0] != 'ndi-ci-smoke'}
    if extras:
        raise ValueError('mobile app resolved dependencies outside the frozen root lockfile')


def native_static_libraries(stderr):
    """Read rustc's ordered link contract without accepting linker instructions."""
    contracts = re.findall(r'^note: native-static-libs: (.*)$', stderr, re.M)
    if len(contracts) != 1 or len(contracts[0]) > 16384:
        raise ValueError('expected one bounded Rust native static library contract')
    flags = contracts[0].split()
    if not flags or len(flags) > 256:
        raise ValueError('empty or oversized Rust native static library contract')
    index = 0
    while index < len(flags):
        flag = flags[index]
        if flag == '-framework':
            index += 1
            if index == len(flags) or not re.fullmatch(r'[A-Za-z][A-Za-z0-9_]{0,127}', flags[index]):
                raise ValueError('invalid framework in Rust native static library contract')
        elif not re.fullmatch(r'-l[A-Za-z0-9_][A-Za-z0-9_+.\-]{0,127}', flag):
            raise ValueError('unsupported argument in Rust native static library contract')
        index += 1
    return flags


def build_static_library(args, output, env):
    result = subprocess.run([str(x) for x in args], capture_output=True, text=True,
                            timeout=1800, env=env)
    # Compiler diagnostics and dependency names remain private runner files.
    (output / 'cargo-stderr.log').write_text(result.stderr, encoding='utf-8')
    (output / 'cargo-artifacts.jsonl').write_text(result.stdout, encoding='utf-8')
    if result.returncode:
        # CellRun captures this subprocess's streams into its private command
        # log. Preserve both Cargo JSON and stderr for its bounded summarizer;
        # only that summary, never the captured log, is printed or uploaded.
        sys.stderr.write(result.stdout)
        sys.stderr.write(result.stderr)
    result.check_returncode()
    write_json(output / 'native-static-libs.json', native_static_libraries(result.stderr))
    return result.stdout


def build_rust(profile, output, build_std):
    project = output / 'rust-app'
    project.mkdir(parents=True)
    kind = 'cdylib' if profile['os'] == 'android' else 'staticlib'
    (project / 'Cargo.toml').write_text('[package]\nname="ndi-ci-smoke"\nversion="0.0.0"\nedition="2021"\n'
        '[lib]\npath="lib.rs"\ncrate-type=[' + json.dumps(kind) + ']\n'
        '[dependencies.grafton-ndi]\npath=' + json.dumps(str(ROOT)) + '\nfeatures=["advanced_sdk","advanced","file-io","tokio","smol","image-encoding"]\n')
    (project / 'lib.rs').write_text('''
fn smoke() -> Result<(), Box<dyn std::error::Error>> {
    use grafton_ndi::{NDI, Sender, SenderOptions, VideoFrame};
    let ndi = NDI::new()?;
    if ndi.info().raw_version().is_empty() { return Err("empty runtime identity".into()); }
    let options = SenderOptions::builder("grafton hosted smoke")
        .clock_video(false).clock_audio(false).build();
    let mut sender = Sender::new(&ndi, &options)?;
    let frame = VideoFrame::builder().resolution(16, 16).build()?;
    let _ = sender.send_video_async(frame);
    drop(sender);
    Ok(())
}
#[no_mangle]
pub extern "C" fn ndi_ci_smoke() -> i32 {
    match std::panic::catch_unwind(smoke) {
        Ok(Ok(())) => 0,
        Ok(Err(error)) => { eprintln!("NDI smoke failed: {error}"); 1 },
        Err(_) => 2,
    }
}
#[cfg(target_os="android")]
#[no_mangle]
pub extern "system" fn Java_video_ndi_grafton_smoke_MainActivity_runSmoke(
    _env: *mut std::ffi::c_void, _class: *mut std::ffi::c_void) -> i32 {
    ndi_ci_smoke()
}
''')
    shutil.copyfile(ROOT / 'Cargo.lock', project / 'Cargo.lock')
    args = ['cargo', 'build' if kind == 'cdylib' else 'rustc', '--offline',
            '--color', 'never', '--message-format=json-render-diagnostics',
            '--manifest-path', project / 'Cargo.toml', '--target', profile['target']]
    if build_std:
        args += ['-Zbuild-std=std,panic_abort']
    if kind == 'staticlib':
        args += ['--lib', '--', '--print=native-static-libs']
    env = os.environ.copy()
    if profile['os'] == 'android':
        existing = env.get('CARGO_ENCODED_RUSTFLAGS', '')
        env['CARGO_ENCODED_RUSTFLAGS'] = (existing + '\x1f' if existing else '') + '-Clink-arg=-Wl,-z,max-page-size=16384'
    if kind == 'staticlib':
        build_log = build_static_library(args, output, env)
    else:
        build_log = call(args, capture=True, timeout=1800, env=env)
    (output / 'cargo-artifacts.jsonl').write_text(build_log, encoding='utf-8')
    check_app_resolution(ROOT / 'Cargo.lock', project / 'Cargo.lock')
    return cargo_artifacts.library(cargo_artifacts.messages(build_log), 'ndi_ci_smoke',
        '.so' if kind == 'cdylib' else '.a', Path(env['CARGO_TARGET_DIR']) / profile['target'])


def android(profile, edition, sdk_root, output, rust_library, execute):
    android_root = Path(os.environ['ANDROID_HOME'])
    build = android_root / 'build-tools/35.0.0'
    android_jar = android_root / 'platforms/android-35/android.jar'
    required = [build / n for n in ('aapt2', 'd8', 'zipalign', 'apksigner')]
    if not all(p.exists() for p in [*required, android_jar]):
        raise ValueError('Android build-tools35.0.0/platform35 must be installed explicitly')
    package = output / 'android-app'
    package.mkdir()
    manifest = package / 'AndroidManifest.xml'
    manifest.write_text(f'''<manifest xmlns:android="http://schemas.android.com/apk/res/android" package="{BUNDLE}">
<uses-sdk android:minSdkVersion="24" android:targetSdkVersion="35"/>
<uses-permission android:name="android.permission.INTERNET"/>
<uses-permission android:name="android.permission.ACCESS_NETWORK_STATE"/>
<uses-permission android:name="android.permission.CHANGE_WIFI_MULTICAST_STATE"/>
<application android:label="NDI CI Smoke" android:debuggable="true" android:extractNativeLibs="true" android:theme="@android:style/Theme.Material.Light.NoActionBar">
<activity android:name=".MainActivity" android:exported="true"><intent-filter>
<action android:name="android.intent.action.MAIN"/><category android:name="android.intent.category.LAUNCHER"/>
</intent-filter></activity></application></manifest>''')
    selected = 'ndi_advanced' if edition == 'advanced' else 'ndi'
    java = package / 'MainActivity.java'
    java.write_text(f'''package {BUNDLE};
import android.app.Activity;
import android.os.Bundle;
import java.io.FileOutputStream;
public class MainActivity extends Activity {{
  static {{ System.loadLibrary("{selected}"); System.loadLibrary("ndi_ci_smoke"); }}
  private static native int runSmoke();
  @Override public void onCreate(Bundle bundle) {{
    super.onCreate(bundle);
    new Thread(() -> {{
      int status = runSmoke();
      try (FileOutputStream out = openFileOutput("ndi-result", MODE_PRIVATE)) {{
        out.write(Integer.toString(status).getBytes(java.nio.charset.StandardCharsets.UTF_8));
      }} catch (Exception failure) {{ throw new RuntimeException(failure); }}
    }}).start();
  }}
}}''')
    classes = package / 'classes'; classes.mkdir()
    dex = package / 'dex'; dex.mkdir()
    call(['javac', '-source', '8', '-target', '8', '-classpath', android_jar, '-d', classes, java])
    call([build / 'd8', '--min-api', '24', '--lib', android_jar, '--output', dex, *classes.rglob('*.class')])
    unsigned = package / 'unsigned.apk'
    call([build / 'aapt2', 'link', '-o', unsigned, '-I', android_jar, '--manifest', manifest])
    vendor = next((sdk_root / n for n in ('lib', 'Lib') if (sdk_root / n).is_dir())) / profile['subdir']
    native = {p.name: p for p in vendor.glob('*.so')}
    if 'lib' + selected + '.so' not in native:
        raise ValueError('selected vendor Android runtime is missing')
    native[rust_library.name] = rust_library
    ndk = Path(os.environ['ANDROID_NDK_HOME']) / 'toolchains/llvm/prebuilt/linux-x86_64/sysroot/usr/lib'
    triple = 'arm-linux-androideabi' if profile['arch'] == 'arm' else profile['target']
    cpp = ndk / triple / 'libc++_shared.so'
    if cpp.exists():
        native[cpp.name] = cpp
    with zipfile.ZipFile(unsigned, 'a') as archive:
        archive.write(dex / 'classes.dex', 'classes.dex')
        for name, path in sorted(native.items()):
            archive.write(path, 'lib/' + profile['subdir'] + '/' + name, compress_type=zipfile.ZIP_STORED)
    aligned = package / 'aligned.apk'
    call([build / 'zipalign', '-P', '16', '-f', '4', unsigned, aligned])
    keystore = package / 'ephemeral.jks'
    call(['keytool', '-genkeypair', '-keystore', keystore, '-storepass', 'android', '-keypass', 'android',
          '-alias', 'smoke', '-dname', 'CN=Local CI Smoke', '-keyalg', 'RSA', '-validity', '2'])
    signed = package / 'smoke.apk'
    call([build / 'apksigner', 'sign', '--ks', keystore, '--ks-pass', 'pass:android', '--out', signed, aligned])
    call([build / 'apksigner', 'verify', '--verbose', signed])
    call([build / 'zipalign', '-c', '-P', '16', '4', signed])
    record = {'package_sha256': digest(signed), 'native_libraries': {n: digest(p) for n, p in native.items()},
              'coverage': 'package', 'min_api': 24, 'target_api': 35}
    if execute:
        adb = android_root / 'platform-tools/adb'
        call([adb, 'wait-for-device'], timeout=180)
        abis = call([adb, 'shell', 'getprop', 'ro.product.cpu.abilist'], capture=True).split(',')
        if profile['subdir'] not in abis:
            raise ValueError('booted emulator does not advertise the required app ABI')
        call([adb, 'install', '-r', signed])
        call([adb, 'shell', 'am', 'start', '-W', '-n', BUNDLE + '/.MainActivity'])
        value = None
        for _ in range(120):
            result = subprocess.run([str(adb), 'shell', 'run-as', BUNDLE, 'cat', 'files/ndi-result'],
                                    capture_output=True, text=True, timeout=10)
            if result.returncode == 0:
                value = result.stdout.strip(); break
            time.sleep(1)
        if value != '0':
            raise ValueError('Android NDI lifecycle app did not report success')
        record.update(coverage='emulator', emulator_abis=abis,
            emulator_api=call([adb, 'shell', 'getprop', 'ro.build.version.sdk'], capture=True),
            emulator_pagesize=call([adb, 'shell', 'getconf', 'PAGE_SIZE'], capture=True),
            emulator_fingerprint=call([adb, 'shell', 'getprop', 'ro.build.fingerprint'], capture=True))
    return record


def choose_simulator(devices, os_name):
    token = 'iOS' if os_name == 'ios' else 'tvOS'
    candidates = [(runtime, d) for runtime, values in devices.items() if token in runtime
                  for d in values if d.get('isAvailable')]
    if not candidates:
        raise ValueError('no available matching hosted Apple simulator runtime')
    runtime, device = sorted(candidates, key=lambda item: (item[0], item[1]['name']))[-1]
    return runtime, device


def smoke_failure(state):
    print('MOBILE_SMOKE_STATE=' + state, file=sys.stderr)
    raise ValueError('Apple simulator smoke did not complete successfully')


def console_tail(path):
    if not path.exists():
        return ''
    with path.open('rb') as stream:
        stream.seek(max(0, path.stat().st_size - 1024 * 1024))
        return stream.read(1024 * 1024).decode('utf-8', errors='replace')


def await_smoke_result(result, process, console, *, timeout=120, interval=1):
    deadline = time.monotonic() + timeout
    state = None
    while True:
        try:
            with result.open('rb') as stream:
                raw = stream.read(33)
            if len(raw) > 32:
                smoke_failure('result-invalid')
            state = raw.decode('ascii').strip()
        except FileNotFoundError:
            state = None
        except (OSError, UnicodeError):
            smoke_failure('result-invalid')
        if state not in (None, 'running', '0', '1', '2'):
            smoke_failure('result-invalid')
        if 'MOBILE_SMOKE_STATE=result-write-failed' in console_tail(console).splitlines():
            smoke_failure('result-write-failed')
        if state == '1':
            smoke_failure('native-error')
        if state == '2':
            smoke_failure('native-panic')
        if process.poll() is not None:
            smoke_failure('app-exited')
        if state == '0':
            return
        if time.monotonic() >= deadline:
            smoke_failure('native-running' if state == 'running' else 'result-missing')
        time.sleep(interval)


def execute_apple_simulator(device, app, output):
    udid = device['udid']
    phase = 'boot'
    process = None
    console = output / 'simulator-console.log'
    try:
        if device['state'] != 'Booted':
            call(['xcrun', 'simctl', 'boot', udid])
        call(['xcrun', 'simctl', 'bootstatus', udid, '-b'], timeout=240)
        phase = 'install'
        call(['xcrun', 'simctl', 'install', udid, app])
        container = Path(call(['xcrun', 'simctl', 'get_app_container', udid, BUNDLE, 'data'], capture=True))
        result = container / 'tmp/ndi-result'
        # Reinstalling an app can preserve its data container. A previous run
        # must never satisfy the current native lifecycle gate.
        result.unlink(missing_ok=True)
        phase = 'launch'
        with console.open('w', encoding='utf-8') as stream:
            try:
                # Apple documents console-pty; unlike console it also avoids
                # the Xcode16.2 large-stderr pipe stall reported by Chromium.
                process = subprocess.Popen(['xcrun', 'simctl', 'launch', '--console-pty',
                    udid, BUNDLE], stdout=stream, stderr=subprocess.STDOUT,
                    stdin=subprocess.DEVNULL, text=True)
            except OSError:
                smoke_failure('launch-failed')
            phase = 'result'
            await_smoke_result(result, process, console)
            phase = 'terminate'
            call(['xcrun', 'simctl', 'terminate', udid, BUNDLE])
            process.wait(timeout=30)
    except BaseException:
        # CellRun keeps this command's streams private and exports only its
        # closed diagnostic vocabulary. Never upload the simulator console.
        sys.stderr.write(console_tail(console))
        print('\nMOBILE_SMOKE_PHASE=' + phase, file=sys.stderr)
        raise
    finally:
        if process is not None and process.poll() is None:
            # Cleanup cannot turn a failed launch/native result into success.
            try:
                subprocess.run(['xcrun', 'simctl', 'terminate', udid, BUNDLE],
                    capture_output=True, timeout=30, check=False)
            except (OSError, subprocess.TimeoutExpired):
                pass
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=5)


def apple(profile, output, rust_library, execute):
    simulator = 'sim' in profile['id']
    os_name = profile['os']
    sdk = {'ios': 'iphonesimulator' if simulator else 'iphoneos',
           'tvos': 'appletvsimulator' if simulator else 'appletvos', 'visionos': 'xros'}[os_name]
    sdkroot = call(['xcrun', '--sdk', sdk, '--show-sdk-path'], capture=True)
    clang = call(['xcrun', '--sdk', sdk, '--find', 'clang'], capture=True)
    version = '1.0' if os_name == 'visionos' else '16.0'
    arch = 'arm64' if profile['arch'] == 'aarch64' else profile['arch']
    target = arch + '-apple-' + {'ios': 'ios', 'tvos': 'tvos', 'visionos': 'xros'}[os_name] + version
    if simulator: target += '-simulator'
    app = output / 'NDISmoke.app'; app.mkdir()
    main = output / 'main.m'
    main.write_text('''#import <UIKit/UIKit.h>
#include <stdio.h>
extern int ndi_ci_smoke(void);
static BOOL WriteSmokeStatus(NSString *status) {
  NSError *error = nil;
  NSString *directory = NSTemporaryDirectory();
  if (directory.length == 0 ||
      ![[NSFileManager defaultManager] createDirectoryAtPath:directory withIntermediateDirectories:YES attributes:nil error:&error] ||
      ![status writeToFile:[directory stringByAppendingPathComponent:@"ndi-result"] atomically:YES encoding:NSUTF8StringEncoding error:&error]) {
    fprintf(stderr, "MOBILE_SMOKE_STATE=result-write-failed\\n");
    fflush(stderr);
    return NO;
  }
  return YES;
}
@interface SmokeSceneDelegate : UIResponder <UIWindowSceneDelegate>
@property(strong, nonatomic) UIWindow *window;
@end
@implementation SmokeSceneDelegate
- (void)scene:(UIScene *)scene willConnectToSession:(UISceneSession *)session options:(UISceneConnectionOptions *)options {
  if (![scene isKindOfClass:[UIWindowScene class]]) { return; }
  self.window = [[UIWindow alloc] initWithWindowScene:(UIWindowScene *)scene];
  self.window.rootViewController = [UIViewController new];
  [self.window makeKeyAndVisible];
}
@end
@interface SmokeDelegate : UIResponder <UIApplicationDelegate>
@end
@implementation SmokeDelegate
- (BOOL)application:(UIApplication *)application didFinishLaunchingWithOptions:(NSDictionary *)options {
  static dispatch_once_t once;
  dispatch_once(&once, ^{
    dispatch_async(dispatch_get_global_queue(QOS_CLASS_USER_INITIATED, 0), ^{
      if (!WriteSmokeStatus(@"running")) { return; }
      int result = ndi_ci_smoke();
      WriteSmokeStatus([NSString stringWithFormat:@"%d", result]);
    });
  });
  return YES;
}
@end
int main(int argc, char **argv) { @autoreleasepool { return UIApplicationMain(argc, argv, nil, @"SmokeDelegate"); } }
''')
    contract = output / 'native-static-libs.json'
    flags = json.loads(contract.read_text(encoding='utf-8'))
    if not isinstance(flags, list) or not all(isinstance(flag, str) for flag in flags):
        raise ValueError('invalid saved Rust native static library contract')
    # Revalidate the stored contract before invoking the final app linker.
    if flags != native_static_libraries('note: native-static-libs: ' + ' '.join(flags)):
        raise ValueError('invalid saved Rust native static library contract')
    call([clang, '-target', target, '-isysroot', sdkroot, '-fobjc-arc', main, rust_library,
          '-framework', 'UIKit', *flags, '-o', app / 'NDISmoke'], timeout=600)
    info = dict(CFBundleIdentifier=BUNDLE, CFBundleExecutable='NDISmoke', CFBundleName='NDISmoke',
        CFBundlePackageType='APPL', CFBundleVersion='1', CFBundleShortVersionString='1.0',
        MinimumOSVersion=version, UIDeviceFamily=[3] if os_name == 'tvos' else [7] if os_name == 'visionos' else [1, 2],
        NSLocalNetworkUsageDescription='Runs the local synthetic NDI validation fixture.',
        NSBonjourServices=['_ndi._tcp'], UILaunchScreen={},
        UIApplicationSceneManifest={
            'UIApplicationSupportsMultipleScenes': False,
            'UISceneConfigurations': {'UIWindowSceneSessionRoleApplication': [{
                'UISceneConfigurationName': 'Smoke', 'UISceneDelegateClassName': 'SmokeSceneDelegate'}]}})
    if os_name == 'visionos':
        info['UIApplicationSceneManifest']['UIApplicationPreferredDefaultSceneSessionRole'] = 'UIWindowSceneSessionRoleApplication'
    with (app / 'Info.plist').open('wb') as f: plistlib.dump(info, f)
    # Simulator signing is ad hoc and creates no device-signing identity.
    call(['codesign', '--force', '--sign', '-', app])
    call(['codesign', '--verify', '--strict', app])
    record = {'coverage': 'package', 'binary_sha256': digest(app / 'NDISmoke'), 'sdk': sdk,
              'clang_target': target, 'native_static_libraries_sha256': digest(contract)}
    if execute:
        devices = json.loads(call(['xcrun', 'simctl', 'list', 'devices', 'available', '--json'], capture=True))['devices']
        runtime, device = choose_simulator(devices, os_name)
        execute_apple_simulator(device, app, output)
        record.update(coverage='simulator', runtime=runtime, device=device['name'])
    return record


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--profile', required=True)
    p.add_argument('--edition', choices=['standard', 'advanced'], required=True)
    p.add_argument('--sdk-root', type=Path, required=True)
    p.add_argument('--output', type=Path, required=True)
    p.add_argument('--build-std', action='store_true')
    p.add_argument('--execute', action='store_true')
    args = p.parse_args()
    profile = profiles()[args.profile]
    args.output.mkdir(parents=True)
    library = build_rust(profile, args.output, args.build_std)
    result = android(profile, args.edition, args.sdk_root, args.output, library, args.execute) if profile['os'] == 'android' else apple(profile, args.output, library, args.execute)
    write_json(args.output / 'mobile.json', result)

if __name__ == '__main__':
    main()

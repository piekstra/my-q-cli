#!/usr/bin/env bash
# Capture the myQ app's API traffic from an arm64 emulator, through mitmproxy,
# with secrets removed by myq_capture.py. The owner's phone is only a source for
# the installed APK; nothing on it changes.
#
#   capture.sh apk      pull the installed APK from the USB-attached phone
#   capture.sh avd      create the emulator (idempotent)
#   capture.sh proxy    run mitmdump in the foreground, logging to the state dir
#   capture.sh boot     start the emulator in the background
#   capture.sh trust    install the proxy CA as a system CA (repeat after each boot)
#   capture.sh install  install the APK into the emulator
#   capture.sh run      set the emulator's proxy and launch the app
#
# State lives outside the repo: $MYQ_CAPTURE_HOME, default
# ~/.local/share/my-q-cli/capture. Nothing there is ever committed.
set -euo pipefail

PKG=com.chamberlain.android.liftmaster.myq
AVD=myq-capture
IMAGE="system-images;android-33;google_apis;arm64-v8a"
PORT=8099
SDK="${ANDROID_HOME:-$HOME/Library/Android/sdk}"
ADB="$SDK/platform-tools/adb"
EMULATOR="$SDK/emulator/emulator"
AVDMANAGER="$SDK/cmdline-tools/latest/bin/avdmanager"
# Current command-line tools need Java 17 or newer.
[ -d /opt/homebrew/opt/openjdk@21 ] && export JAVA_HOME="${JAVA_HOME:-/opt/homebrew/opt/openjdk@21}"
HOME_DIR="${MYQ_CAPTURE_HOME:-$HOME/.local/share/my-q-cli/capture}"
MITM_DIR="$HOME_DIR/mitm"
APK_DIR="$HOME_DIR/apk"
HERE="$(cd "$(dirname "$0")" && pwd)"

mkdir -p "$HOME_DIR" "$MITM_DIR" "$APK_DIR"
chmod 700 "$HOME_DIR" "$MITM_DIR"

# The emulator is the only attached device a step may touch, except `apk`.
emu() { "$ADB" -e "$@"; }

case "${1:-}" in
apk)
  serial="${2:?usage: capture.sh apk <phone-serial>}"
  rm -f "$APK_DIR"/*.apk
  for p in $("$ADB" -s "$serial" shell pm path "$PKG" | sed 's/package://' | tr -d '\r'); do
    "$ADB" -s "$serial" pull "$p" "$APK_DIR/" >/dev/null
  done
  ls "$APK_DIR"/*.apk >/dev/null 2>&1 || { echo "no APK was pulled from $serial" >&2; exit 1; }
  ls -la "$APK_DIR"
  ;;
avd)
  if "$EMULATOR" -list-avds | grep -qx "$AVD"; then
    echo "$AVD exists"
  else
    echo no | "$AVDMANAGER" create avd -n "$AVD" -k "$IMAGE" -d pixel_6
  fi
  ;;
proxy)
  out="$HOME_DIR/flows-$(date +%Y%m%d-%H%M%S).jsonl"
  echo "logging redacted flows to $out"
  # Google's own hosts carry no myQ API and pin their certificates; pass them through.
  MYQ_CAPTURE_OUT="$out" exec mitmdump --quiet \
    --listen-port "$PORT" \
    --set confdir="$MITM_DIR" \
    --ignore-hosts '(^|\.)(google\.com|googleapis\.com|gstatic\.com|android\.com|gvt[0-9]\.com|googleusercontent\.com)(:\d+)?$' \
    -s "$HERE/myq_capture.py"
  ;;
boot)
  nohup "$EMULATOR" -avd "$AVD" -no-snapshot -no-boot-anim -no-audio -gpu host >"$HOME_DIR/emulator.log" 2>&1 &
  emu wait-for-device
  until [ "$(emu shell getprop sys.boot_completed 2>/dev/null | tr -d '\r')" = "1" ]; do sleep 2; done
  echo "emulator booted"
  ;;
trust)
  ca="$MITM_DIR/mitmproxy-ca-cert.pem"
  [ -f "$ca" ] || { echo "no CA yet: run 'capture.sh proxy' first" >&2; exit 1; }
  name="$(openssl x509 -inform PEM -subject_hash_old -in "$ca" -noout).0"
  emu root >/dev/null
  emu wait-for-device
  emu push "$ca" "/data/local/tmp/$name" >/dev/null
  # A tmpfs over the system store holds the originals plus the proxy CA. It
  # needs no writable system image and disappears at the next boot.
  emu shell "mkdir -p /data/local/tmp/cacerts && cp /system/etc/security/cacerts/* /data/local/tmp/cacerts/ \
    && mount -t tmpfs tmpfs /system/etc/security/cacerts \
    && cp /data/local/tmp/cacerts/* /system/etc/security/cacerts/ \
    && cp /data/local/tmp/$name /system/etc/security/cacerts/$name \
    && chown root:root /system/etc/security/cacerts/* \
    && chmod 644 /system/etc/security/cacerts/* \
    && chcon u:object_r:system_file:s0 /system/etc/security/cacerts/*"
  emu shell "ls /system/etc/security/cacerts | grep -c $name"
  ;;
install)
  ls "$APK_DIR"/*.apk >/dev/null 2>&1 || { echo "no APK yet: run 'capture.sh apk <phone-serial>' first" >&2; exit 1; }
  emu install-multiple -r "$APK_DIR"/*.apk
  ;;
run)
  emu shell settings put global http_proxy "10.0.2.2:$PORT"
  emu shell am force-stop "$PKG"
  emu shell monkey -p "$PKG" -c android.intent.category.LAUNCHER 1 >/dev/null
  echo "launched $PKG through the proxy"
  ;;
*)
  sed -n '2,15p' "$0"
  exit 2
  ;;
esac

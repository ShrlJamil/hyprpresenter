/// Groundwork for the planned OTA-style update checker.
///
/// A future build will, on demand (and later automatically at start-up):
///   1. GET [releasesLatestUrl] and read the release `tag_name`
///      (format `vX.Y.Z`).
///   2. Compare it with this build's version from `package_info_plus`
///      (the pubspec `version:` — shown on the start screen).
///   3. If newer, offer the release's [apkAssetName] asset for download
///      and hand it to the Android installer.
///
/// The asset name is contractual: every GitHub release must attach the
/// arm64 APK under exactly this file name.
library;

const String updateRepo = 'ShrlJamil/hyprpresenter';
const String releasesLatestUrl =
    'https://api.github.com/repos/$updateRepo/releases/latest';
const String apkAssetName = 'app-arm64-v8a-release.apk';

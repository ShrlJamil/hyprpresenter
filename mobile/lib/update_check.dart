/// OTA-style update checker backed by GitHub Releases.
///
/// How it works:
///   1. GET [releasesLatestUrl] and read the release `tag_name`
///      (format `vX.Y.Z`).
///   2. Compare it with this build's version from `package_info_plus`
///      (the pubspec `version:` — shown on the start screen).
///   3. If newer, the start screen offers the release's [apkAssetName]
///      asset; tapping it opens the download in the browser, and the
///      user installs the APK from there.
///
/// The asset name is contractual: every GitHub release must attach the
/// arm64 APK under exactly this file name.
library;

import 'dart:convert';
import 'dart:io';

import 'package:package_info_plus/package_info_plus.dart';

const String updateRepo = 'ShrlJamil/hyprpresenter';
const String releasesLatestUrl =
    'https://api.github.com/repos/$updateRepo/releases/latest';
const String apkAssetName = 'hyprpresenter-arm64.apk';

/// A release that is newer than the installed build.
class UpdateInfo {
  const UpdateInfo({
    required this.version,
    required this.downloadUrl,
    required this.notes,
  });

  /// Display version without the leading "v" ("0.2.0").
  final String version;

  /// Direct download URL of the [apkAssetName] asset.
  final String downloadUrl;

  /// Release notes (the GitHub release body), may be empty.
  final String notes;
}

enum UpdateCheckStatus { upToDate, updateAvailable, failed }

class UpdateCheckResult {
  const UpdateCheckResult(this.status, [this.info]);

  final UpdateCheckStatus status;
  final UpdateInfo? info;
}

/// "v0.2.0" / "0.2.0" → [0, 2, 0]; null when unparsable.
List<int>? parseVersion(String raw) {
  final s = raw.trim().replaceFirst(RegExp(r'^[vV]'), '');
  if (s.isEmpty) return null;
  final parts = s.split('.');
  if (parts.length > 4) return null;
  final out = <int>[];
  for (final p in parts) {
    final n = int.tryParse(p);
    if (n == null) return null;
    out.add(n);
  }
  return out;
}

/// True when [candidate] (a release tag) is strictly newer than
/// [current] (the installed version).  Unparsable input is never
/// treated as newer.
bool isNewerVersion(String candidate, String current) {
  final c = parseVersion(candidate);
  final u = parseVersion(current);
  if (c == null || u == null) return false;
  final len = c.length > u.length ? c.length : u.length;
  for (var i = 0; i < len; i++) {
    final a = i < c.length ? c[i] : 0;
    final b = i < u.length ? u[i] : 0;
    if (a != b) return a > b;
  }
  return false;
}

/// The update offer inside a GitHub "latest release" payload, or null
/// when the release is not newer or carries no [apkAssetName] asset.
UpdateInfo? updateFromReleaseJson(
  Map<String, dynamic> json,
  String currentVersion,
) {
  final tag = json['tag_name'];
  if (tag is! String || !isNewerVersion(tag, currentVersion)) return null;
  final assets = json['assets'];
  if (assets is! List) return null;
  for (final a in assets) {
    if (a is Map &&
        a['name'] == apkAssetName &&
        a['browser_download_url'] is String) {
      return UpdateInfo(
        version: tag.replaceFirst(RegExp(r'^[vV]'), ''),
        downloadUrl: a['browser_download_url'] as String,
        notes: (json['body'] as String?) ?? '',
      );
    }
  }
  return null;
}

/// Ask GitHub for the latest release and compare it with the
/// installed build.  Never throws: network and parsing problems come
/// back as [UpdateCheckStatus.failed].
Future<UpdateCheckResult> checkForUpdate() async {
  try {
    final info = await PackageInfo.fromPlatform();
    final client = HttpClient()
      ..connectionTimeout = const Duration(seconds: 10);
    try {
      final req = await client.getUrl(Uri.parse(releasesLatestUrl));
      req.headers.set(HttpHeaders.acceptHeader, 'application/vnd.github+json');
      req.headers.set(
        HttpHeaders.userAgentHeader,
        'HyprPresenter/${info.version}',
      );
      final resp = await req.close().timeout(const Duration(seconds: 15));
      if (resp.statusCode != 200) {
        return const UpdateCheckResult(UpdateCheckStatus.failed);
      }
      final body = await resp.transform(utf8.decoder).join();
      final decoded = jsonDecode(body);
      if (decoded is! Map<String, dynamic>) {
        return const UpdateCheckResult(UpdateCheckStatus.failed);
      }
      final update = updateFromReleaseJson(decoded, info.version);
      return update == null
          ? const UpdateCheckResult(UpdateCheckStatus.upToDate)
          : UpdateCheckResult(UpdateCheckStatus.updateAvailable, update);
    } finally {
      client.close();
    }
  } catch (_) {
    return const UpdateCheckResult(UpdateCheckStatus.failed);
  }
}

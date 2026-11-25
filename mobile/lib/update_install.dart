/// In-app installation of a downloaded update.
///
/// The companion to update_check.dart: once a newer release is known,
/// this downloads its APK inside the app and hands it to the Android
/// system installer — no browser detour.  Android still shows its own
/// install confirmation, and the very first install needs the user to
/// allow "install unknown apps" for HyprPresenter once (the settings
/// page [UpdateInstaller.openInstallPermissionSettings] opens).
library;

import 'dart:io';

import 'package:flutter/services.dart';
import 'package:path_provider/path_provider.dart';

import 'update_check.dart';

/// Platform bridge to the Android installer APIs (MainActivity.kt).
class UpdateInstaller {
  static const MethodChannel _channel =
      MethodChannel('hyprpresenter/update_installer');

  /// Whether Android currently lets this app install packages.
  static Future<bool> canInstallPackages() async {
    final ok = await _channel.invokeMethod<bool>('canInstallPackages');
    return ok ?? false;
  }

  /// Open the system settings page where the user allows installs
  /// from this app.
  static Future<void> openInstallPermissionSettings() async {
    await _channel.invokeMethod<void>('openInstallPermissionSettings');
  }

  /// Hand the APK at [path] to the system installer.
  static Future<void> installApk(String path) async {
    await _channel.invokeMethod<void>('installApk', {'path': path});
  }
}

/// Download the APK of [info] into the app's cache directory,
/// reporting progress as (receivedBytes, totalBytes) — total is null
/// when the server sends no content length.  Returns the file ready
/// for [UpdateInstaller.installApk].  Throws on any failure; the
/// partial file is removed.
Future<File> downloadUpdateApk(
  UpdateInfo info, {
  void Function(int received, int? total)? onProgress,
}) async {
  final cache = await getTemporaryDirectory();
  final dir = Directory('${cache.path}/update');
  await dir.create(recursive: true);
  final file = File('${dir.path}/hyprpresenter-${info.version}.apk');
  final client = HttpClient()
    ..connectionTimeout = const Duration(seconds: 10);
  IOSink? sink;
  try {
    final req = await client.getUrl(Uri.parse(info.downloadUrl));
    req.headers.set(HttpHeaders.userAgentHeader, 'HyprPresenter');
    final resp = await req.close();
    if (resp.statusCode != 200) {
      throw HttpException('Download failed (HTTP ${resp.statusCode})');
    }
    final total = resp.contentLength >= 0 ? resp.contentLength : null;
    var received = 0;
    sink = file.openWrite();
    await for (final chunk in resp) {
      sink.add(chunk);
      received += chunk.length;
      onProgress?.call(received, total);
    }
    await sink.close();
    sink = null;
    return file;
  } catch (_) {
    await sink?.close();
    if (await file.exists()) await file.delete();
    rethrow;
  } finally {
    client.close();
  }
}

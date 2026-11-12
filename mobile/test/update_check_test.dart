import 'package:flutter_test/flutter_test.dart';
import 'package:hyprpresenter_mobile/update_check.dart';

void main() {
  group('parseVersion', () {
    test('strips a leading v and splits components', () {
      expect(parseVersion('v0.2.0'), [0, 2, 0]);
      expect(parseVersion('0.1.0'), [0, 1, 0]);
      expect(parseVersion(' 1.2 '), [1, 2]);
    });

    test('rejects unparsable input', () {
      expect(parseVersion(''), isNull);
      expect(parseVersion('v'), isNull);
      expect(parseVersion('1.x.0'), isNull);
      expect(parseVersion('1.2.3.4.5'), isNull);
    });
  });

  group('isNewerVersion', () {
    test('compares numerically per component', () {
      expect(isNewerVersion('v0.2.0', '0.1.0'), isTrue);
      expect(isNewerVersion('v0.10.0', '0.9.9'), isTrue);
      expect(isNewerVersion('v1.0.0', '0.9.9'), isTrue);
    });

    test('equal or older is not newer', () {
      expect(isNewerVersion('v0.1.0', '0.1.0'), isFalse);
      expect(isNewerVersion('v0.1.0', '0.2.0'), isFalse);
      expect(isNewerVersion('garbage', '0.1.0'), isFalse);
    });
  });

  group('updateFromReleaseJson', () {
    Map<String, dynamic> release(String tag, {bool withAsset = true}) => {
      'tag_name': tag,
      'body': 'notes',
      'assets': [
        if (withAsset)
          {
            'name': apkAssetName,
            'browser_download_url': 'https://example.com/$apkAssetName',
          },
      ],
    };

    test('newer release with the APK asset yields an offer', () {
      final info = updateFromReleaseJson(release('v0.2.0'), '0.1.0');
      expect(info, isNotNull);
      expect(info!.version, '0.2.0');
      expect(info.downloadUrl, endsWith(apkAssetName));
    });

    test('same version yields no offer', () {
      expect(updateFromReleaseJson(release('v0.1.0'), '0.1.0'), isNull);
    });

    test('newer release without the asset yields no offer', () {
      expect(
        updateFromReleaseJson(release('v0.2.0', withAsset: false), '0.1.0'),
        isNull,
      );
    });
  });
}

import 'package:flutter_test/flutter_test.dart';
import 'package:hyprpresenter_mobile/pairing.dart';

void main() {
  test('parses a desktop pairing payload', () {
    final values = parsePairingUri(
      'hyprpresenter://pair?host=192.168.1.7&port=8423&token=ab12cd34',
    );
    expect(values, isNotNull);
    expect(values!.host, '192.168.1.7');
    expect(values.port, 8423);
    expect(values.token, 'ab12cd34');
  });

  test('rejects foreign QR payloads', () {
    expect(parsePairingUri('https://example.com'), isNull);
    expect(parsePairingUri('hyprpresenter://pair?host=192.168.1.7'), isNull);
    expect(parsePairingUri('not a uri at all'), isNull);
  });
}

/// Parsing for the desktop's pairing QR payload:
/// `hyprpresenter://pair?host=<ip>&port=<port>&token=<hex>`
/// (produced by `hyprpresenter pair` on the desktop).
library;

/// The three values a pairing payload carries.
typedef PairingValues = ({String host, int port, String token});

/// Decode a scanned string into pairing values, or null when it is not a
/// HyprPresenter pairing payload.
PairingValues? parsePairingUri(String raw) {
  final uri = Uri.tryParse(raw.trim());
  if (uri == null || uri.scheme != 'hyprpresenter' || uri.host != 'pair') {
    return null;
  }
  final host = uri.queryParameters['host'];
  final port = int.tryParse(uri.queryParameters['port'] ?? '');
  final token = uri.queryParameters['token'];
  if (host == null || host.isEmpty || port == null || token == null) {
    return null;
  }
  final cleanToken = token.replaceAll(RegExp(r'\s+'), '');
  if (cleanToken.isEmpty) return null;
  return (host: host, port: port, token: cleanToken);
}

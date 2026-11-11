import 'package:flutter/material.dart';
import 'package:mobile_scanner/mobile_scanner.dart';

import 'pairing.dart';

/// Full-screen camera scanner for the desktop pairing QR.
///
/// Route contract: pops with the parsed [PairingValues] of the first
/// valid HyprPresenter QR, or null when the user backs out.  The caller
/// (PairingGate) pushes this route as `MaterialPageRoute<PairingValues>`;
/// popping with any other type throws at runtime, so keep the two in sync.
/// Foreign QR codes are ignored — the scanner stays open.
class QrScanScreen extends StatefulWidget {
  const QrScanScreen({super.key});

  @override
  State<QrScanScreen> createState() => _QrScanScreenState();
}

class _QrScanScreenState extends State<QrScanScreen> {
  bool _handled = false;

  void _onDetect(BarcodeCapture capture) {
    if (_handled) return;
    for (final barcode in capture.barcodes) {
      final value = barcode.rawValue;
      if (value == null) continue;
      final parsed = parsePairingUri(value);
      if (parsed == null) continue; // Not ours: keep scanning.
      _handled = true;
      Navigator.of(context).pop<PairingValues>(parsed);
      return;
    }
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(title: const Text('Scan QR')),
      body: MobileScanner(onDetect: _onDetect),
    );
  }
}

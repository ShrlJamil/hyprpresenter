import 'package:flutter/material.dart';
import 'package:mobile_scanner/mobile_scanner.dart';

/// Full-screen camera scanner for the desktop pairing QR.  Pops with the
/// first decoded string, or null when the user backs out.
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
      if (value != null) {
        _handled = true;
        Navigator.of(context).pop(value);
        return;
      }
    }
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(title: const Text('Scan pairing QR')),
      body: Stack(
        children: [
          MobileScanner(onDetect: _onDetect),
          const Align(
            alignment: Alignment.bottomCenter,
            child: Padding(
              padding: EdgeInsets.all(24),
              child: Text(
                'Point the camera at the QR shown by `hyprpresenter pair` '
                'on your desktop.',
                textAlign: TextAlign.center,
                style: TextStyle(
                  color: Colors.white,
                  shadows: [Shadow(blurRadius: 8, color: Colors.black)],
                ),
              ),
            ),
          ),
        ],
      ),
    );
  }
}

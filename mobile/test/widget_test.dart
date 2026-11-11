import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:shared_preferences/shared_preferences.dart';

import 'package:hyprpresenter_mobile/main.dart';

void main() {
  testWidgets('first launch is QR-only: one scan button, no form fields', (
    tester,
  ) async {
    SharedPreferences.setMockInitialValues({});
    await tester.pumpWidget(const HyprPresenterApp());
    await tester.pumpAndSettle();
    expect(find.text('Scan QR'), findsOneWidget);
    expect(find.byType(TextField), findsNothing);
    expect(find.text('Connect'), findsNothing);
  });
}

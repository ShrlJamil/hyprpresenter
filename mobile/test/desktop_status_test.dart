import 'package:flutter_test/flutter_test.dart';
import 'package:hyprpresenter_mobile/desktop_status.dart';

void main() {
  test('parses a dual-monitor status line', () {
    final status = parseDesktopStatus(
      'outputs=eDP-1,DP-1 dot=2240,540 on=DP-1 visible=1 radius=12 '
      'color=#ff0000 confined=DP-1 '
      'areas=eDP-1:-256,120,1536,960;DP-1:1280,0,1920,1080 '
      'keyboard=ready buffers=eDP-1:1920x1200@1.250 configured=2/2',
    );
    expect(status, isNotNull);
    expect(status!.outputs.length, 2);
    expect(status.outputs.first.name, 'eDP-1');
    expect(status.outputs.first.x, -256);
    expect(status.outputs[1].centerX, 2240);
    expect(status.outputs[1].centerY, 540);
    expect(status.confined, 'DP-1');
    expect(status.onOutput, 'DP-1');
    expect(status.dotX, 2240);
    final b = status.bounds;
    expect((b.minX, b.minY, b.maxX, b.maxY), (-256, 0, 3200, 1080));
    expect(status.outputNamed('DP-1')!.width, 1920);
    expect(status.outputNamed('nope'), isNull);
  });

  test('unconfined parses to null; garbage parses to null', () {
    final status = parseDesktopStatus(
      'dot=512,600 on=eDP-1 visible=1 radius=12 color=#ff0000 '
      'confined=none areas=eDP-1:-256,120,1536,960',
    );
    expect(status!.confined, isNull);
    expect(status.outputs.length, 1);
    expect(parseDesktopStatus('ok dot=1,2'), isNull);
    expect(parseDesktopStatus('error: something'), isNull);
  });
}

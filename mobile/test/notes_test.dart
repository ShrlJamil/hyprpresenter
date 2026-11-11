import 'package:flutter_test/flutter_test.dart';
import 'package:hyprpresenter_mobile/notes.dart';

void main() {
  group('slide notes markdown', () {
    test('serialize/parse round-trips, keeping empty slides', () {
      final notes = ['Pembuka: sapa dulu.', '', 'Angka penting: 42.'];
      expect(parseSlideNotes(serializeSlideNotes(notes)), notes);
    });

    test('numbered headings are honoured and gaps become empty notes', () {
      final notes = parseSlideNotes('## 1\nPertama\n\n## 3\nKetiga\n');
      expect(notes, ['Pertama', '', 'Ketiga']);
    });

    test('"Slide N" headings are accepted', () {
      final notes = parseSlideNotes('## Slide 1\nA\n## Slide 2\nB\n');
      expect(notes, ['A', 'B']);
    });

    test('separator-only documents split in order', () {
      final notes = parseSlideNotes('Satu\n---\nDua\n---\nTiga\n');
      expect(notes, ['Satu', 'Dua', 'Tiga']);
    });

    test('a plain document is one note for slide 1', () {
      expect(parseSlideNotes('Halo dunia\n'), ['Halo dunia']);
      expect(parseSlideNotes('  \n'), isEmpty);
    });
  });
}

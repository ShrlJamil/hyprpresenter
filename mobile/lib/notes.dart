import 'dart:convert';
import 'dart:io';
import 'dart:typed_data';

import 'package:file_picker/file_picker.dart';
import 'package:path_provider/path_provider.dart';

/// Per-slide presenter notes.
///
/// The interchange format is plain Markdown: one `## N` heading per
/// slide, the note text below it.  The app generates and edits this
/// format itself (see [NotesEditorPage]), so users never have to learn
/// it — but because it is just a text file, notes can also be prepared
/// or edited on a laptop and opened through [pickNotesFile].  The
/// parser is deliberately forgiving: `## 1`, `## Slide 1`, and `---`
/// separators are all accepted.

final RegExp _headingPattern = RegExp(
  r'^#{1,6}\s*(?:slide\s*)?(\d+)\s*$',
  caseSensitive: false,
);
final RegExp _separatorPattern = RegExp(r'^\s*(?:-{3,}|\*{3,})\s*$');

/// Parse a notes document into one entry per slide (index 0 = slide 1).
///
/// Numbered headings win when present: missing numbers become empty
/// notes, so numbering never shifts.  Without headings, `---`
/// separators split the document in order.  A document with neither is
/// a single note for slide 1.
List<String> parseSlideNotes(String source) {
  final lines = source.split('\n');

  final byNumber = <int, List<String>>{};
  int? current;
  for (final line in lines) {
    final match = _headingPattern.firstMatch(line.trim());
    if (match != null) {
      current = int.parse(match.group(1)!);
      byNumber.putIfAbsent(current, () => <String>[]);
    } else if (current != null) {
      byNumber[current]!.add(line);
    }
  }
  if (byNumber.isNotEmpty) {
    var highest = 0;
    for (final n in byNumber.keys) {
      if (n > highest) highest = n;
    }
    return <String>[
      for (var i = 1; i <= highest; i++)
        (byNumber[i] ?? const <String>[]).join('\n').trim(),
    ];
  }

  if (lines.any(_separatorPattern.hasMatch)) {
    final parts = <List<String>>[<String>[]];
    for (final line in lines) {
      if (_separatorPattern.hasMatch(line)) {
        parts.add(<String>[]);
      } else {
        parts.last.add(line);
      }
    }
    final notes = parts.map((p) => p.join('\n').trim()).toList();
    while (notes.isNotEmpty && notes.first.isEmpty) {
      notes.removeAt(0);
    }
    while (notes.isNotEmpty && notes.last.isEmpty) {
      notes.removeLast();
    }
    return notes;
  }

  final whole = source.trim();
  return whole.isEmpty ? <String>[] : <String>[whole];
}

/// Render notes back to the Markdown interchange format.
String serializeSlideNotes(List<String> notes) {
  final buffer = StringBuffer();
  for (var i = 0; i < notes.length; i++) {
    if (i > 0) buffer.writeln();
    buffer.writeln('## ${i + 1}');
    buffer.writeln();
    final body = notes[i].trim();
    if (body.isNotEmpty) {
      buffer.writeln(body);
      buffer.writeln();
    }
  }
  return '${buffer.toString().trimRight()}\n';
}

/// The notes' home inside the app: a Markdown file in the app's
/// documents directory — the same format as an exported file, so the
/// internal copy and the portable copy can never drift in format.
abstract final class NotesStore {
  static Future<File> _notesFile() async {
    final dir = await getApplicationDocumentsDirectory();
    return File('${dir.path}/slide_notes.md');
  }

  static Future<List<String>> load() async {
    try {
      final file = await _notesFile();
      if (await file.exists()) {
        return parseSlideNotes(await file.readAsString());
      }
    } catch (_) {
      // A broken local copy must never block presenting.
    }
    return <String>[];
  }

  static Future<void> save(List<String> notes) async {
    try {
      final file = await _notesFile();
      await file.writeAsString(serializeSlideNotes(notes));
    } catch (_) {
      // Best effort: editing keeps working in memory regardless.
    }
  }
}

/// Let the user pick an existing notes file (.md / .txt) and parse it.
/// Returns null when the picker was cancelled or the file unreadable.
Future<List<String>?> pickNotesFile() async {
  try {
    final files = await FilePicker.pickFiles(
      type: FileType.custom,
      allowedExtensions: const ['md', 'txt'],
    );
    if (files.isEmpty) return null;
    final bytes = await files.first.readAsBytes();
    return parseSlideNotes(utf8.decode(bytes));
  } catch (_) {
    // Cancelled or unreadable: the caller keeps the current notes.
    return null;
  }
}

/// Offer the notes as a saveable Markdown file (system save dialog).
/// Returns true when the user actually saved somewhere.
Future<bool> exportNotesFile(List<String> notes) async {
  try {
    final bytes = Uint8List.fromList(utf8.encode(serializeSlideNotes(notes)));
    final uri = await FilePicker.saveFile(
      dialogTitle: 'Save slide notes',
      fileName: 'slide-notes.md',
      mimeType: 'text/markdown',
      bytes: bytes,
    );
    return uri != null;
  } catch (_) {
    return false;
  }
}

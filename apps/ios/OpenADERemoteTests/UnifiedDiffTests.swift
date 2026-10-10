import XCTest
@testable import OpenADERemote

final class UnifiedDiffTests: XCTestCase {
    func testLineNumbersFollowBothSidesAndResetForEachHunk() {
        let patch = """
        diff --git a/View.swift b/View.swift
        --- a/View.swift
        +++ b/View.swift
        @@ -3,2 +3,3 @@
         let kept = true
        -old()
        +new()
        +extra()
        @@ -20 +21 @@
        -before()
        +after()
        """
        let file = UnifiedDiffDocument.parse(patch).files[0]
        XCTAssertEqual(file.path, "View.swift")
        XCTAssertEqual(file.hunks.count, 2)
        XCTAssertEqual(file.hunks[0].lines.map(\.oldLineNumber), [3, 4, nil, nil])
        XCTAssertEqual(file.hunks[0].lines.map(\.newLineNumber), [3, nil, 4, 5])
        XCTAssertEqual(file.hunks[1].lines.map(\.oldLineNumber), [20, nil])
        XCTAssertEqual(file.hunks[1].lines.map(\.newLineNumber), [nil, 21])
        XCTAssertEqual(file.addedCount, 3)
        XCTAssertEqual(file.removedCount, 2)
    }

    func testFileMarkersInsideHunksAreChangesAndMissingNewlineDoesNotAdvanceNumbers() {
        let lines = DiffLine.parse("--- a/file\n+++ b/file\n@@ -1,2 +1,2 @@\n---old\n+++new\n\\ No newline at end of file\n kept")
        XCTAssertEqual(lines[3].kind, .removed)
        XCTAssertEqual(lines[3].content, "--old")
        XCTAssertEqual(lines[4].kind, .added)
        XCTAssertEqual(lines[4].content, "++new")
        XCTAssertEqual(lines[5].kind, .header)
        XCTAssertNil(lines[5].newLineNumber)
        XCTAssertEqual(lines[6].oldLineNumber, 2)
        XCTAssertEqual(lines[6].newLineNumber, 2)
    }

    func testNewAndDeletedFilesNeverInventOppositeSideLineNumbers() {
        let patch = """
        diff --git a/new.swift b/new.swift
        new file mode 100644
        --- /dev/null
        +++ b/new.swift
        @@ -0,0 +1 @@
        +let value = true
        diff --git a/old.swift b/old.swift
        deleted file mode 100644
        --- a/old.swift
        +++ /dev/null
        @@ -8 +0,0 @@
        -let old = false
        """
        let files = UnifiedDiffDocument.parse(patch).files
        XCTAssertEqual(files.count, 2)
        XCTAssertNil(files[0].hunks[0].lines[0].oldLineNumber)
        XCTAssertEqual(files[0].hunks[0].lines[0].newLineNumber, 1)
        XCTAssertNil(files[1].hunks[0].lines[0].newLineNumber)
        XCTAssertEqual(files[1].hunks[0].lines[0].oldLineNumber, 8)
        XCTAssertTrue(files[1].isDeleted)
        XCTAssertNil(files[1].newPath)
    }

    func testQuotedGitPathsDecodeUTF8OctalBytesWithoutLosingSpaces() {
        let patch = #"diff --git "a/caf\303\251 name.swift" "b/caf\303\251 name.swift""# + "\n"
            + #"--- "a/caf\303\251 name.swift""# + "\n"
            + #"+++ "b/caf\303\251 name.swift""# + "\n@@ -1 +1 @@\n-old\n+new"
        XCTAssertEqual(UnifiedDiffDocument.parse(patch).files.first?.path, "café name.swift")
        let binary = UnifiedDiffDocument.parse("diff --git a/Images/new logo.png b/Images/new logo.png\nBinary files a/Images/new logo.png and b/Images/new logo.png differ")
        XCTAssertEqual(binary.files.first?.path, "Images/new logo.png")
        XCTAssertTrue(binary.files[0].isBinary)
        XCTAssertEqual(binary.addedCount, 0)
    }

    func testCommentsNameOriginalSideForDeletedLinesAndCurrentSideForAddedLines() throws {
        let lines = DiffLine.parse("@@ -5 +8 @@\n-removed()\n+added()")
        let old = try XCTUnwrap(lines[1].commentContext(path: "src/File.swift", comment: "¿Se necesita conservar?"))
        let new = try XCTUnwrap(lines[2].commentContext(path: "src/File.swift", comment: "Agregá una prueba."))
        XCTAssertTrue(old.contains("Anterior · L5"))
        XCTAssertTrue(old.contains("versión anterior, línea eliminada"))
        XCTAssertFalse(old.contains("L8"))
        XCTAssertTrue(new.contains("Nueva · L8"))
        XCTAssertTrue(new.contains("> added()"))
        XCTAssertNil(lines[0].commentContext(path: "src/File.swift", comment: "metadata"))
        XCTAssertNil(lines[2].commentContext(path: "src/File.swift", comment: " \n "))
    }

    func testMalformedHunksAndIntegerOverflowNeverProduceMisleadingCommentLocations() {
        let malformed = DiffLine.parse("@@ not a valid range @@\n+code")
        XCTAssertNil(malformed.last?.newLineNumber)
        XCTAssertNil(malformed.last?.locationTitle)
        let overflow = DiffLine.parse("@@ -0,0 +\(Int.max),2 @@\n+first\n+second")
        XCTAssertEqual(overflow[1].newLineNumber, Int.max)
        XCTAssertNil(overflow[2].newLineNumber)
        XCTAssertEqual(UnifiedDiffDocument.parse("").files, [])
    }
}

// Writes the .DS_Store that lays out the disk image window, without Finder: the window size,
// the background picture and where DSA.app and the Applications link sit.
//
//   swift make-dmg-store.swift <mounted volume>
//
// The volume is the read-write image build-app.sh mounted (hdiutil attach -nobrowse), and it
// holds DSA.app, the Applications link and .background/background.tiff. The background is
// named by an alias record (version 2), which Finder resolves by the volume's name and
// creation date and the file's catalog ID, so the store is written against the mounted
// volume and the image is compressed afterwards without changing either.
import Foundation

let windowWidth = 600, windowHeight = 400
let iconSize = 128
let icons = [("Applications", 450, 190), ("DSA.app", 150, 190)]
let backgroundPath = ".background/background.tiff"

guard CommandLine.arguments.count == 2 else {
    FileHandle.standardError.write("usage: make-dmg-store.swift <mounted volume>\n".data(using: .utf8)!)
    exit(2)
}
let volume = URL(fileURLWithPath: CommandLine.arguments[1]).standardizedFileURL

func fail(_ message: String) -> Never {
    FileHandle.standardError.write("make-dmg-store: \(message)\n".data(using: .utf8)!)
    exit(1)
}

struct Writer {
    var data = Data()
    mutating func u8(_ v: UInt8) { data.append(v) }
    mutating func u16(_ v: UInt16) { withUnsafeBytes(of: v.bigEndian) { data.append(contentsOf: $0) } }
    mutating func u32(_ v: UInt32) { withUnsafeBytes(of: v.bigEndian) { data.append(contentsOf: $0) } }
    mutating func bytes(_ d: Data) { data.append(d) }
    mutating func ascii(_ s: String) { data.append(s.data(using: .ascii)!) }
    mutating func pascal(_ s: String, size: Int) {
        var d = (s.data(using: .macOSRoman) ?? Data()).prefix(size - 1)
        u8(UInt8(d.count))
        d.append(Data(count: size - 1 - d.count))
        bytes(d)
    }
    mutating func utf16(_ s: String) {
        let units = Array(s.utf16)
        u32(UInt32(units.count))
        for unit in units { u16(unit) }
    }
}

// Seconds since 1904, the HFS epoch.
func hfsDate(_ date: Date) -> UInt32 { UInt32(max(0, date.timeIntervalSince1970 + 2_082_844_800)) }
func catalogID(_ url: URL) -> UInt32 {
    var info = stat()
    guard stat(url.path, &info) == 0 else { fail("cannot stat \(url.path)") }
    return UInt32(truncatingIfNeeded: info.st_ino)
}

func aliasRecord() -> Data {
    let target = volume.appendingPathComponent(backgroundPath)
    let parent = target.deletingLastPathComponent()
    let volumeValues = try? volume.resourceValues(forKeys: [.volumeNameKey, .volumeCreationDateKey])
    guard let volumeName = volumeValues?.volumeName, let volumeDate = volumeValues?.volumeCreationDate,
          let targetDate = try? target.resourceValues(forKeys: [.creationDateKey]).creationDate
    else { fail("cannot read the volume name, its creation date or the background's") }

    var w = Writer()
    w.u32(0) // application information
    w.u16(0) // record length, filled in below
    w.u16(2) // version
    w.u16(0) // a file
    w.pascal(volumeName, size: 28)
    w.u32(hfsDate(volumeDate))
    w.ascii("H+")
    w.u16(5) // an ejectable disk
    w.u32(catalogID(parent))
    w.pascal(target.lastPathComponent, size: 64)
    w.u32(catalogID(target))
    w.u32(hfsDate(targetDate))
    w.u32(0); w.u32(0) // creator and type
    w.u16(0xFFFF); w.u16(0xFFFF) // levels from and to: unknown
    w.u32(0) // volume attributes
    w.u16(0) // volume file system ID
    w.bytes(Data(count: 10))

    func tag(_ id: UInt16, _ value: Data) {
        w.u16(id)
        w.u16(UInt16(value.count))
        w.bytes(value)
        if value.count % 2 == 1 { w.u8(0) }
    }
    func unicode(_ s: String) -> Data {
        var t = Writer()
        let units = Array(s.utf16)
        t.u16(UInt16(units.count))
        for unit in units { t.u16(unit) }
        return t.data
    }
    tag(0, parent.lastPathComponent.data(using: .macOSRoman)!) // parent folder name
    var path = Writer()
    path.u32(catalogID(parent))
    tag(1, path.data) // catalog IDs of the folders above the target
    tag(2, "\(volumeName):\(backgroundPath.replacingOccurrences(of: "/", with: ":"))".data(using: .macOSRoman)!)
    tag(14, unicode(target.lastPathComponent))
    tag(15, unicode(volumeName))
    tag(18, "/\(backgroundPath)".data(using: .utf8)!)
    tag(19, volume.path.data(using: .utf8)!)
    w.u16(0xFFFF); w.u16(0)

    var record = w.data
    let length = UInt16(record.count).bigEndian
    withUnsafeBytes(of: length) { record.replaceSubrange(4..<6, with: $0) }
    return record
}

func plist(_ value: [String: Any]) -> Data {
    guard let d = try? PropertyListSerialization.data(fromPropertyList: value, format: .binary, options: 0)
    else { fail("cannot encode a property list") }
    return d
}

// The records, sorted the way Finder keeps them: by name, ignoring case, then by code.
typealias Record = (name: String, code: String, type: String, value: Data)
var records: [Record] = []
records.append((".", "bwsp", "blob", plist([
    "ContainerShowSidebar": false, "ShowPathbar": false, "ShowSidebar": false,
    "ShowStatusBar": false, "ShowTabView": false, "ShowToolbar": false,
    "WindowBounds": "{{200, 120}, {\(windowWidth), \(windowHeight)}}",
])))
records.append((".", "icvp", "blob", plist([
    "arrangeBy": "none", "backgroundColorBlue": 1.0, "backgroundColorGreen": 1.0,
    "backgroundColorRed": 1.0, "backgroundImageAlias": aliasRecord(), "backgroundType": 2,
    "gridOffsetX": 0.0, "gridOffsetY": 0.0, "gridSpacing": 100.0, "iconSize": Double(iconSize),
    "labelOnBottom": true, "showIconPreview": true, "showItemInfo": false, "textSize": 13.0,
    "viewOptionsVersion": 1,
])))
var version = Writer(); version.u32(1)
records.append((".", "vSrn", "long", version.data))
for (name, x, y) in icons {
    var location = Writer()
    location.u32(UInt32(x)); location.u32(UInt32(y))
    location.bytes(Data([0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0, 0]))
    records.append((name, "Iloc", "blob", location.data))
}
records.sort { a, b in
    let order = a.name.lowercased().compare(b.name.lowercased())
    return order == .orderedSame ? a.code < b.code : order == .orderedAscending
}

// The one leaf node of the B-tree.
var leaf = Writer()
leaf.u32(0) // a leaf
leaf.u32(UInt32(records.count))
for record in records {
    leaf.utf16(record.name)
    leaf.ascii(record.code)
    leaf.ascii(record.type)
    if record.type == "blob" { leaf.u32(UInt32(record.value.count)) }
    leaf.bytes(record.value)
}
guard leaf.data.count <= 0x1000 else { fail("the records do not fit one 4 KiB node") }

// The buddy allocator's file: offsets count from byte 4. Block 0 is the allocator's own
// information at 0x800 (2 KiB), block 1 the B-tree header at 0x20 (32 bytes) and block 2
// the leaf at 0x1000 (4 KiB). Every other power-of-two span is on a free list.
let infoOffset: UInt32 = 0x800, dsdbOffset: UInt32 = 0x20, leafOffset: UInt32 = 0x1000
var info = Writer()
info.u32(3); info.u32(0)
let addresses = [infoOffset | 11, dsdbOffset | 5, leafOffset | 12]
for i in 0..<256 { info.u32(i < addresses.count ? addresses[i] : 0) }
info.u32(1); info.u8(4); info.ascii("DSDB"); info.u32(1)
var free = [[UInt32]](repeating: [], count: 32)
for k in 6...10 { free[k].append(1 << UInt32(k)) }
for k in 13...30 { free[k].append(1 << UInt32(k)) }
for list in free {
    info.u32(UInt32(list.count))
    for offset in list { info.u32(offset) }
}

var dsdb = Writer()
dsdb.u32(2); dsdb.u32(0); dsdb.u32(UInt32(records.count)); dsdb.u32(1); dsdb.u32(0x1000)

var file = Data(count: 4 + 0x2000)
func put(_ d: Data, at offset: UInt32) { file.replaceSubrange(Int(4 + offset)..<Int(4 + offset) + d.count, with: d) }
var header = Writer()
header.u32(1); header.ascii("Bud1")
header.u32(infoOffset); header.u32(0x800); header.u32(infoOffset)
header.bytes(Data([0x00, 0x00, 0x10, 0x0C, 0x00, 0x00, 0x00, 0x87, 0x00, 0x00, 0x20, 0x0B, 0x00, 0x00, 0x00, 0x00]))
file.replaceSubrange(0..<header.data.count, with: header.data)
put(dsdb.data, at: dsdbOffset)
put(info.data, at: infoOffset)
put(leaf.data, at: leafOffset)

do {
    try file.write(to: volume.appendingPathComponent(".DS_Store"))
} catch {
    fail("cannot write the .DS_Store: \(error)")
}
print("Wrote \(volume.path)/.DS_Store with \(records.count) records")

import Foundation

struct PositionPacket {
    let sequence: UInt32
    let timestampUnix: Int64
    let latitudeE7: Int32
    let longitudeE7: Int32
    let altitudeCm: Int32
    let speedCms: UInt16
    let courseCdeg: UInt16
    let accuracyCm: UInt32
    let flags: UInt8

    func encode() -> Data {
        var bytes = [UInt8]()
        bytes.reserveCapacity(34)
        bytes.append(1) // Version 1
        bytes.append(flags)
        bytes.append(contentsOf: sequence.littleEndianBytes)
        bytes.append(contentsOf: timestampUnix.littleEndianBytes)
        bytes.append(contentsOf: latitudeE7.littleEndianBytes)
        bytes.append(contentsOf: longitudeE7.littleEndianBytes)
        bytes.append(contentsOf: altitudeCm.littleEndianBytes)
        bytes.append(contentsOf: speedCms.littleEndianBytes)
        bytes.append(contentsOf: courseCdeg.littleEndianBytes)
        bytes.append(contentsOf: accuracyCm.littleEndianBytes)
        return Data(bytes)
    }
}

extension FixedWidthInteger {
    var littleEndianBytes: [UInt8] {
        withUnsafeBytes(of: self.littleEndian) { Array($0) }
    }
}

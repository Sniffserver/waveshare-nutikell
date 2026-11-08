import CoreBluetooth
import Combine
import CryptoKit
import Foundation

enum OutdoorUUID {
    static let service = CBUUID(string: "7A7E0001-5B8A-4C9C-9A4C-6B5E5F001001")
    static let position = CBUUID(string: "7A7E0002-5B8A-4C9C-9A4C-6B5E5F001001")
    static let control = CBUUID(string: "7A7E0003-5B8A-4C9C-9A4C-6B5E5F001001")
    static let status = CBUUID(string: "7A7E0004-5B8A-4C9C-9A4C-6B5E5F001001")
    static let snapshot = CBUUID(string: "7A7E0005-5B8A-4C9C-9A4C-6B5E5F001001")
}

final class BLEManager: NSObject, ObservableObject, CBCentralManagerDelegate, CBPeripheralDelegate {
    @Published var stateText = "Bluetooth unavailable"
    @Published var connected = false
    @Published var lastSequence: UInt32 = 0
    @Published var snapshotState = "Key not provisioned"

    private var central: CBCentralManager!
    private var watch: CBPeripheral?
    private var positionCharacteristic: CBCharacteristic?
    private var statusCharacteristic: CBCharacteristic?
    private var snapshotCharacteristic: CBCharacteristic?
    private var positionKey: SymmetricKey?
    private var snapshotSequence: UInt32 = 0
    private var pendingSnapshotTransfer: Data?
    private var pendingSnapshotOffset = 0
    private var pendingSnapshotFragmentLength = 0
    private var snapshotWritePending = false

    var canSyncSnapshot: Bool {
        connected && positionKey != nil && snapshotCharacteristic != nil && pendingSnapshotTransfer == nil
    }

    override init() {
        super.init()
        central = CBCentralManager(delegate: self, queue: .main)
    }

    /// Set the per-device key obtained from the provisioning flow/keychain.
    /// The app intentionally has no built-in fallback key.
    func configurePositionKey(_ keyData: Data) {
        guard keyData.count >= 16 else {
            stateText = "Invalid position key"
            return
        }
        positionKey = SymmetricKey(data: keyData)
        snapshotState = "Ready"
    }

    func centralManagerDidUpdateState(_ central: CBCentralManager) {
        switch central.state {
        case .poweredOn:
            stateText = "Scanning"
            central.scanForPeripherals(withServices: [OutdoorUUID.service], options: nil)
        case .poweredOff:
            stateText = "Bluetooth is off"
        case .unauthorized:
            stateText = "Bluetooth permission denied"
        default:
            stateText = "Bluetooth unavailable"
        }
    }

    func centralManager(
        _ central: CBCentralManager,
        didDiscover peripheral: CBPeripheral,
        advertisementData: [String: Any],
        rssi RSSI: NSNumber
    ) {
        watch = peripheral
        watch?.delegate = self
        central.stopScan()
        stateText = "Connecting"
        central.connect(peripheral)
    }

    func centralManager(_ central: CBCentralManager, didConnect peripheral: CBPeripheral) {
        connected = true
        stateText = "Discovering services"
        peripheral.discoverServices([OutdoorUUID.service])
    }

    func centralManager(
        _ central: CBCentralManager,
        didDisconnectPeripheral peripheral: CBPeripheral,
        error: Error?
    ) {
        connected = false
        stateText = "Disconnected"
        positionCharacteristic = nil
        statusCharacteristic = nil
        snapshotCharacteristic = nil
        clearSnapshotTransfer()
        snapshotState = "Disconnected"
        // Auto-reconnect scan
        central.scanForPeripherals(withServices: [OutdoorUUID.service], options: nil)
    }

    func peripheral(_ peripheral: CBPeripheral, didDiscoverServices error: Error?) {
        guard error == nil else {
            stateText = "Service discovery failed"
            return
        }
        guard let service = peripheral.services?.first(where: { $0.uuid == OutdoorUUID.service }) else {
            stateText = "Outdoor service missing"
            return
        }
        peripheral.discoverCharacteristics(
            [OutdoorUUID.position, OutdoorUUID.status, OutdoorUUID.snapshot],
            for: service
        )
    }

    func peripheral(
        _ peripheral: CBPeripheral,
        didDiscoverCharacteristicsFor service: CBService,
        error: Error?
    ) {
        guard error == nil else {
            stateText = "Characteristic discovery failed"
            return
        }
        for characteristic in service.characteristics ?? [] {
            switch characteristic.uuid {
            case OutdoorUUID.position:
                positionCharacteristic = characteristic
                stateText = "Ready"
            case OutdoorUUID.status:
                statusCharacteristic = characteristic
                peripheral.setNotifyValue(true, for: characteristic)
            case OutdoorUUID.snapshot:
                snapshotCharacteristic = characteristic
                snapshotState = positionKey == nil ? "Key not provisioned" : "Ready"
            default:
                break
            }
        }
    }

    func send(_ packet: PositionPacket) {
        guard let watch, let characteristic = positionCharacteristic,
              let positionKey else {
            stateText = "Position key not provisioned"
            return
        }
        guard pendingSnapshotTransfer == nil else { return }
        let data = packet.encode(authenticatedWith: positionKey)
        guard data.count == 50 else {
            stateText = "Invalid packet length"
            return
        }
        watch.writeValue(data, for: characteristic, type: .withResponse)
        lastSequence = packet.sequence
    }

    @discardableResult
    func sendSnapshot(_ json: Data) -> Bool {
        guard let watch, snapshotCharacteristic != nil, let positionKey else {
            snapshotState = "Key or BLE service unavailable"
            return false
        }
        guard !json.isEmpty, json.count <= 12 * 1024 else {
            snapshotState = "Snapshot size rejected"
            return false
        }
        guard pendingSnapshotTransfer == nil else {
            snapshotState = "Snapshot already sending"
            return false
        }

        let wallClockSequence = UInt32(truncatingIfNeeded: Date().timeIntervalSince1970)
        snapshotSequence = wallClockSequence == snapshotSequence ? snapshotSequence &+ 1 : wallClockSequence

        var authenticated = Data("sbie-watch.sync.v1\0".utf8)
        authenticated.appendLittleEndian(snapshotSequence)
        authenticated.append(json)
        let tag = HMAC<SHA256>.authenticationCode(for: authenticated, using: positionKey)
        var transfer = json
        transfer.append(contentsOf: tag.prefix(16))

        guard transfer.count <= Int(UInt16.max), watch.maximumWriteValueLength(for: .withResponse) > 10 else {
            snapshotState = "BLE packet limit too small"
            return false
        }

        pendingSnapshotTransfer = transfer
        pendingSnapshotOffset = 0
        snapshotWritePending = false
        snapshotState = "Sending snapshot"
        writeNextSnapshotFragment()
        return true
    }

    func peripheral(
        _ peripheral: CBPeripheral,
        didWriteValueFor characteristic: CBCharacteristic,
        error: Error?
    ) {
        guard characteristic.uuid == OutdoorUUID.snapshot else { return }
        snapshotWritePending = false
        guard error == nil else {
            clearSnapshotTransfer()
            snapshotState = "Snapshot write failed"
            return
        }

        pendingSnapshotOffset += pendingSnapshotFragmentLength
        pendingSnapshotFragmentLength = 0
        writeNextSnapshotFragment()
    }

    private func writeNextSnapshotFragment() {
        guard !snapshotWritePending,
              let watch,
              let characteristic = snapshotCharacteristic,
              let transfer = pendingSnapshotTransfer else { return }

        if pendingSnapshotOffset == transfer.count {
            clearSnapshotTransfer()
            snapshotState = "Synced"
            return
        }

        let payloadLimit = watch.maximumWriteValueLength(for: .withResponse) - 10
        guard payloadLimit > 0 else {
            clearSnapshotTransfer()
            snapshotState = "BLE packet limit too small"
            return
        }
        let fragmentLength = min(payloadLimit, transfer.count - pendingSnapshotOffset)

        var frame = Data([0x53, 1])
        frame.appendLittleEndian(snapshotSequence)
        frame.appendLittleEndian(UInt16(transfer.count))
        frame.appendLittleEndian(UInt16(pendingSnapshotOffset))
        frame.append(transfer[pendingSnapshotOffset..<(pendingSnapshotOffset + fragmentLength)])
        pendingSnapshotFragmentLength = fragmentLength
        snapshotWritePending = true
        watch.writeValue(frame, for: characteristic, type: .withResponse)
    }

    private func clearSnapshotTransfer() {
        pendingSnapshotTransfer = nil
        pendingSnapshotOffset = 0
        pendingSnapshotFragmentLength = 0
        snapshotWritePending = false
    }
}

private extension Data {
    mutating func appendLittleEndian<Value: FixedWidthInteger>(_ value: Value) {
        var littleEndianValue = value.littleEndian
        Swift.withUnsafeBytes(of: &littleEndianValue) { append(contentsOf: $0) }
    }
}

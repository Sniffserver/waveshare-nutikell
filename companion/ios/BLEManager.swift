import CoreBluetooth
import Foundation

enum OutdoorUUID {
    static let service = CBUUID(string: "7A7E0001-5B8A-4C9C-9A4C-6B5E5F001001")
    static let position = CBUUID(string: "7A7E0002-5B8A-4C9C-9A4C-6B5E5F001001")
    static let control = CBUUID(string: "7A7E0003-5B8A-4C9C-9A4C-6B5E5F001001")
    static let status = CBUUID(string: "7A7E0004-5B8A-4C9C-9A4C-6B5E5F001001")
}

final class BLEManager: NSObject, ObservableObject, CBCentralManagerDelegate, CBPeripheralDelegate {
    @Published var stateText = "Bluetooth unavailable"
    @Published var connected = false
    @Published var lastSequence: UInt32 = 0

    private var central: CBCentralManager!
    private var watch: CBPeripheral?
    private var positionCharacteristic: CBCharacteristic?
    private var statusCharacteristic: CBCharacteristic?

    override init() {
        super.init()
        central = CBCentralManager(delegate: self, queue: .main)
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
        peripheral.discoverCharacteristics([OutdoorUUID.position, OutdoorUUID.status], for: service)
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
            default:
                break
            }
        }
    }

    func send(_ packet: PositionPacket) {
        guard let watch, let characteristic = positionCharacteristic else { return }
        let data = packet.encode()
        guard data.count == 34 else {
            stateText = "Invalid packet length"
            return
        }
        watch.writeValue(data, for: characteristic, type: .withResponse)
        lastSequence = packet.sequence
    }
}

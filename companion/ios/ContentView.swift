import SwiftUI
import CoreLocation

struct ContentView: View {
    @EnvironmentObject var ble: BLEManager
    @EnvironmentObject var location: LocationProvider
    @State private var sequence: UInt32 = 0

    var body: some View {
        NavigationStack {
            VStack(spacing: 20) {
                Text("Outdoor Companion")
                    .font(.largeTitle.bold())

                StatusRow(
                    title: "Watch BLE",
                    value: ble.connected ? "Connected" : ble.stateText,
                    color: ble.connected ? .green : .orange
                )

                StatusRow(
                    title: "GPS Location",
                    value: location.lastLocation == nil ? "No fix" : "Fix available",
                    color: location.lastLocation == nil ? .orange : .green
                )

                if let position = location.lastLocation {
                    VStack(alignment: .leading, spacing: 8) {
                        Text("Latitude:  \(position.coordinate.latitude, specifier: "%.6f")°")
                        Text("Longitude: \(position.coordinate.longitude, specifier: "%.6f")°")
                        Text("Accuracy:  ±\(position.horizontalAccuracy, specifier: "%.1f") m")
                        Text("Speed:     \(max(position.speed, 0) * 3.6, specifier: "%.1f") km/h")
                        if position.course >= 0 {
                            Text("Course:    \(position.course, specifier: "%.0f")°")
                        } else {
                            Text("Course:    Stationary")
                        }
                    }
                    .font(.system(.body, design: .monospaced))
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding()
                    .background(Color(.secondarySystemBackground))
                    .clipShape(RoundedRectangle(cornerRadius: 16))
                }

                if !location.authorized {
                    Button("Grant Location Permission") {
                        location.requestPermission()
                    }
                    .buttonStyle(.borderedProminent)
                }

                Button(location.tracking ? "Stop Live Sync" : "Start Live Sync") {
                    if location.tracking {
                        location.stop()
                    } else {
                        location.start()
                    }
                }
                .buttonStyle(.borderedProminent)
                .tint(location.tracking ? .red : .green)
                .controlSize(.large)

                Spacer()

                Text("Transmitted seq: #\(ble.lastSequence)")
                    .font(.footnote)
                    .foregroundStyle(.secondary)
            }
            .padding()
            .onAppear {
                connectLocationToBLE()
            }
        }
    }

    private func connectLocationToBLE() {
        location.onLocation = { position in
            guard ble.connected else { return }
            sequence &+= 1

            let hasCourse = position.course >= 0
            let hasAltitude = position.altitude >= 0
            let hasSpeed = position.speed >= 0
            let hasAccuracy = position.horizontalAccuracy >= 0

            var flags: UInt8 = 0x01 // COORDS_VALID
            if hasAltitude { flags |= 0x02 }
            if hasSpeed { flags |= 0x04 }
            if hasCourse { flags |= 0x08 }
            if hasAccuracy { flags |= 0x10 }

            let packet = PositionPacket(
                sequence: sequence,
                timestampUnix: Int64(position.timestamp.timeIntervalSince1970),
                latitudeE7: Int32(position.coordinate.latitude * 10_000_000),
                longitudeE7: Int32(position.coordinate.longitude * 10_000_000),
                altitudeCm: Int32(max(position.altitude, 0) * 100),
                speedCms: UInt16(min(max(position.speed, 0) * 100, 65535)),
                courseCdeg: UInt16(hasCourse ? min(max(position.course, 0) * 100, 35999) : 0),
                accuracyCm: UInt32(min(max(position.horizontalAccuracy, 0) * 100, 4_000_000)),
                flags: flags
            )
            ble.send(packet)
        }
    }
}

struct StatusRow: View {
    let title: String
    let value: String
    let color: Color

    var body: some View {
        HStack {
            Circle()
                .fill(color)
                .frame(width: 12, height: 12)
            Text(title)
                .fontWeight(.semibold)
            Spacer()
            Text(value)
                .foregroundStyle(.secondary)
        }
    }
}

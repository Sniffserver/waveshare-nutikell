import SwiftUI

@main
struct OutdoorCompanionApp: App {
    @StateObject private var ble = BLEManager()
    @StateObject private var location = LocationProvider()
    @StateObject private var snapshots = SnapshotProvider()

    var body: some Scene {
        WindowGroup {
            ContentView()
                .environmentObject(ble)
                .environmentObject(location)
                .environmentObject(snapshots)
        }
    }
}

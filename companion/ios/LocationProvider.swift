import CoreLocation
import Foundation

final class LocationProvider: NSObject, ObservableObject, CLLocationManagerDelegate {
    private let manager = CLLocationManager()
    @Published var authorized = false
    @Published var lastLocation: CLLocation?
    @Published var tracking = false
    var onLocation: ((CLLocation) -> Void)?

    override init() {
        super.init()
        manager.delegate = self
        manager.desiredAccuracy = kCLLocationAccuracyBest
        manager.distanceFilter = 5.0 // 5m filter to conserve power
        manager.activityType = .fitness
        manager.pausesLocationUpdatesAutomatically = true
    }

    func requestPermission() {
        manager.requestWhenInUseAuthorization()
    }

    func start() {
        manager.startUpdatingLocation()
        tracking = true
    }

    func stop() {
        manager.stopUpdatingLocation()
        tracking = false
    }

    func locationManagerDidChangeAuthorization(_ manager: CLLocationManager) {
        authorized = (manager.authorizationStatus == .authorizedWhenInUse || manager.authorizationStatus == .authorizedAlways)
    }

    func locationManager(_ manager: CLLocationManager, didUpdateLocations locations: [CLLocation]) {
        guard let location = locations.last else { return }
        guard location.horizontalAccuracy >= 0 else { return }
        lastLocation = location
        onLocation?(location)
    }
}

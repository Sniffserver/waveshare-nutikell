import CoreLocation
import Combine
import EventKit
import Foundation
import WeatherKit

@MainActor
final class SnapshotProvider: ObservableObject {
    @Published var stateText = "Idle"

    private let eventStore = EKEventStore()

    func makeSnapshot(for location: CLLocation) async -> Data? {
        stateText = "Checking calendar access"
        guard await hasCalendarAccess() else {
            stateText = "Calendar access needed"
            return nil
        }

        let now = Date()
        let agenda = makeAgenda(from: now)
        let weather: SyncWeather?
        do {
            weather = try await makeWeather(for: location)
        } catch {
            weather = nil
        }

        let snapshot = SyncSnapshot(
            schema: 1,
            generatedAtUnix: Int64(now.timeIntervalSince1970),
            weather: weather,
            agenda: agenda
        )
        let encoder = JSONEncoder()
        encoder.keyEncodingStrategy = .convertToSnakeCase
        encoder.outputFormatting = [.sortedKeys]

        do {
            let data = try encoder.encode(snapshot)
            guard data.count <= 12 * 1024 else {
                stateText = "Snapshot too large"
                return nil
            }
            stateText = weather == nil ? "Calendar snapshot ready" : "Snapshot ready"
            return data
        } catch {
            stateText = "Snapshot encoding failed"
            return nil
        }
    }

    private func hasCalendarAccess() async -> Bool {
        if #available(iOS 17.0, *) {
            switch EKEventStore.authorizationStatus(for: .event) {
            case .fullAccess:
                return true
            case .notDetermined:
                return (try? await eventStore.requestFullAccessToEvents()) == true
            default:
                return false
            }
        }

        switch EKEventStore.authorizationStatus(for: .event) {
        case .authorized:
            return true
        case .notDetermined:
            return (try? await eventStore.requestAccess(to: .event)) == true
        default:
            return false
        }
    }

    private func makeWeather(for location: CLLocation) async throws -> SyncWeather {
        let weather = try await WeatherService.shared.weather(for: location)
        let current = weather.currentWeather
        let hourly = weather.hourlyForecast.forecast.prefix(24).map { hour in
            SyncHourlyForecast(
                startUnix: Int64(hour.date.timeIntervalSince1970),
                temperatureTenthsC: tenthsCelsius(hour.temperature),
                condition: syncCondition(hour.condition),
                precipitationProbabilityPercent: percent(hour.precipitationChance)
            )
        }
        let today = weather.dailyForecast.forecast.first
        let currentTemperature = tenthsCelsius(current.temperature)
        let high = today.map { tenthsCelsius($0.highTemperature) } ?? currentTemperature
        let low = today.map { tenthsCelsius($0.lowTemperature) } ?? currentTemperature

        return SyncWeather(
            location: "Here",
            observedAtUnix: Int64(Date().timeIntervalSince1970),
            temperatureTenthsC: currentTemperature,
            condition: syncCondition(current.condition),
            highTenthsC: max(high, low),
            lowTenthsC: min(high, low),
            precipitationProbabilityPercent: hourly.first?.precipitationProbabilityPercent ?? 0,
            hourly: hourly
        )
    }

    private func makeAgenda(from now: Date) -> SyncAgenda {
        let end = Calendar.current.date(byAdding: .day, value: 2, to: now)
            ?? now.addingTimeInterval(48 * 60 * 60)
        let predicate = eventStore.predicateForEvents(withStart: now, end: end, calendars: nil)
        let sortedEvents = eventStore.events(matching: predicate).sorted {
            $0.startDate < $1.startDate
        }

        let events = sortedEvents.prefix(20).enumerated().map { index, event in
            let start = Int64(event.startDate.timeIntervalSince1970)
            return SyncCalendarEvent(
                id: "\(start)-\(index)",
                title: boundedUTF8(event.title ?? "Untitled", maxBytes: 64),
                startUnix: start,
                endUnix: Int64(event.endDate.timeIntervalSince1970),
                location: boundedUTF8(event.location ?? "", maxBytes: 64),
                allDay: event.isAllDay
            )
        }
        return SyncAgenda(events: events)
    }

    private func syncCondition(_ condition: WeatherKit.WeatherCondition) -> SyncCondition {
        let name = String(describing: condition).lowercased()
        if name.contains("thunder") || name.contains("storm") { return .thunderstorm }
        if name.contains("snow") || name.contains("flurr") || name.contains("blizzard") { return .snow }
        if name.contains("rain") || name.contains("drizzle") || name.contains("sleet") { return .rain }
        if name.contains("fog") || name.contains("haze") || name.contains("smok") { return .fog }
        if name.contains("partly") { return .partlyCloudy }
        if name.contains("cloud") { return .cloudy }
        if name.contains("clear") { return .clear }
        return .unknown
    }

    private func tenthsCelsius(_ temperature: Measurement<UnitTemperature>) -> Int16 {
        let value = Int((temperature.converted(to: .celsius).value * 10).rounded())
        return Int16(min(max(value, -900), 700))
    }

    private func percent(_ value: Double) -> UInt8 {
        UInt8(min(max(Int((value * 100).rounded()), 0), 100))
    }

    private func boundedUTF8(_ value: String, maxBytes: Int) -> String {
        var result = ""
        var count = 0
        for scalar in value.unicodeScalars {
            let scalarBytes = String(scalar).utf8.count
            guard count + scalarBytes <= maxBytes else { break }
            result.unicodeScalars.append(scalar)
            count += scalarBytes
        }
        return result
    }
}

private enum SyncCondition: String, Encodable {
    case clear
    case partlyCloudy = "partly_cloudy"
    case cloudy
    case rain
    case snow
    case thunderstorm
    case fog
    case unknown
}

private struct SyncHourlyForecast: Encodable {
    let startUnix: Int64
    let temperatureTenthsC: Int16
    let condition: SyncCondition
    let precipitationProbabilityPercent: UInt8
}

private struct SyncWeather: Encodable {
    let location: String
    let observedAtUnix: Int64
    let temperatureTenthsC: Int16
    let condition: SyncCondition
    let highTenthsC: Int16
    let lowTenthsC: Int16
    let precipitationProbabilityPercent: UInt8
    let hourly: [SyncHourlyForecast]
}

private struct SyncCalendarEvent: Encodable {
    let id: String
    let title: String
    let startUnix: Int64
    let endUnix: Int64
    let location: String
    let allDay: Bool
}

private struct SyncAgenda: Encodable {
    let events: [SyncCalendarEvent]
}

private struct SyncSnapshot: Encodable {
    let schema: UInt16
    let generatedAtUnix: Int64
    let weather: SyncWeather?
    let agenda: SyncAgenda

    enum CodingKeys: String, CodingKey {
        case schema
        case generatedAtUnix
        case weather
        case agenda
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(schema, forKey: .schema)
        try container.encode(generatedAtUnix, forKey: .generatedAtUnix)
        if let weather {
            try container.encode(weather, forKey: .weather)
        } else {
            try container.encodeNil(forKey: .weather)
        }
        try container.encode(agenda, forKey: .agenda)
    }
}

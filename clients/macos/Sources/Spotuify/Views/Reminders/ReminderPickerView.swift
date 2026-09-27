import SwiftUI
import SpotuifyKit

/// Sheet to schedule a listening reminder for a media item: quick-pick presets
/// set the date, a graphical picker allows a custom time, plus a recurrence
/// choice and an optional note.
struct ReminderPickerView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.room) private var room
    @Environment(\.dismiss) private var dismiss
    let item: MediaItem

    @State private var date: Date = ReminderPreset.tomorrowMorning.resolve()
    @State private var recurrence: Recurrence = .none
    @State private var message: String = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            header

            RoomSectionLabel("When")
            LazyVGrid(columns: Array(repeating: GridItem(.flexible()), count: 3), spacing: 8) {
                ForEach(ReminderPreset.allCases, id: \.self) { preset in
                    Button {
                        date = preset.resolve()
                    } label: {
                        Text(preset.label)
                            .frame(maxWidth: .infinity)
                    }
                    .buttonStyle(RoomButtonStyle())
                }
            }

            DatePicker("Custom", selection: $date, in: Date()...)
                .datePickerStyle(.compact)

            RoomSectionLabel("Repeat")
            RoomTabs(
                options: Recurrence.allCases.map { (value: $0, title: $0.label) },
                selection: $recurrence)

            TextField("Note (optional)", text: $message)
                .glassField()

            HStack {
                Spacer()
                Button("Cancel") { dismiss() }
                    .buttonStyle(RoomButtonStyle())
                Button("Set Reminder") {
                    model.createReminder(
                        uri: item.uri,
                        anchorAtMs: Int64(date.timeIntervalSince1970 * 1000),
                        recurrence: recurrence,
                        message: message.isEmpty ? nil : message)
                    dismiss()
                }
                .buttonStyle(RoomButtonStyle(kind: .primary))
                .disabled(date <= Date())
            }
        }
        .padding(24)
        .frame(width: 440)
        .background { ZStack { room.base; Grain() }.ignoresSafeArea() }
    }

    private var header: some View {
        HStack(spacing: 12) {
            AsyncCoverImage(url: item.imageURL, cornerRadius: 6)
                .frame(width: 48, height: 48)
            VStack(alignment: .leading, spacing: 2) {
                MonoCaps("Remind me about", size: 9.5, color: room.accent)
                Text(item.name).font(.displayTitle(19)).foregroundStyle(room.ink).lineLimit(1)
                if !item.subtitle.isEmpty {
                    Text(item.subtitle).font(.system(size: 12)).foregroundStyle(room.inkMuted).lineLimit(1)
                }
            }
            Spacer()
        }
    }
}

/// Quick-pick reminder times resolved in the user's local timezone.
enum ReminderPreset: CaseIterable {
    case laterToday, thisEvening, tomorrowMorning, thisWeekend, nextWeek, inAnHour

    var label: String {
        switch self {
        case .inAnHour: "In 1 hour"
        case .laterToday: "Later today"
        case .thisEvening: "This evening"
        case .tomorrowMorning: "Tomorrow 9am"
        case .thisWeekend: "This weekend"
        case .nextWeek: "Next week"
        }
    }

    func resolve(now: Date = Date(), calendar: Calendar = .current) -> Date {
        let startOfToday = calendar.startOfDay(for: now)
        func at(_ hour: Int, _ day: Date) -> Date {
            calendar.date(bySettingHour: hour, minute: 0, second: 0, of: day) ?? day
        }
        switch self {
        case .inAnHour:
            return now.addingTimeInterval(3600)
        case .laterToday:
            let six = at(18, startOfToday)
            return now < six ? six : at(9, calendar.date(byAdding: .day, value: 1, to: startOfToday)!)
        case .thisEvening:
            return at(19, startOfToday)
        case .tomorrowMorning:
            return at(9, calendar.date(byAdding: .day, value: 1, to: startOfToday)!)
        case .thisWeekend:
            let weekday = calendar.component(.weekday, from: startOfToday) // 1=Sun…7=Sat
            let daysUntilSat = (7 - weekday + 7) % 7
            let sat = calendar.date(
                byAdding: .day, value: daysUntilSat == 0 ? 7 : daysUntilSat, to: startOfToday)!
            return at(10, sat)
        case .nextWeek:
            return at(9, calendar.date(byAdding: .day, value: 7, to: startOfToday)!)
        }
    }
}

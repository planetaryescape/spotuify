import SwiftUI
import SpotuifyKit

/// The Notifications page: an Inbox of fired reminders (Play / Queue / Snooze /
/// Dismiss) plus the list of Scheduled reminders (cancel).
struct RemindersView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.room) private var room

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            EditorialPageHeader("Notifications", eyebrow: "You")
            scroll
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
        .navigationTitle("Notifications")
        .task { await model.reminders.loadAll() }
    }

    private var scroll: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 8, pinnedViews: [.sectionHeaders]) {
                let inbox = model.reminders.openNotifications
                Section {
                    if inbox.isEmpty {
                        emptyRow("No new reminders", systemImage: "bell.slash")
                    } else {
                        ForEach(inbox) { NotificationRow(notification: $0) }
                    }
                } header: {
                    sectionHeader("Inbox", count: inbox.count)
                }

                let scheduled = model.reminders.reminders
                Section {
                    if scheduled.isEmpty {
                        emptyRow("Nothing scheduled", systemImage: "calendar")
                    } else {
                        ForEach(scheduled) { ReminderRow(reminder: $0) }
                    }
                } header: {
                    sectionHeader("Scheduled", count: scheduled.count)
                }
            }
            .padding(.horizontal, 24).padding(.bottom, 24)
        }
    }

    private func sectionHeader(_ title: String, count: Int) -> some View {
        RoomSectionLabel(count > 0 ? "\(title) · \(count)" : title)
            .roomPinnedBackground()
    }

    private func emptyRow(_ text: String, systemImage: String) -> some View {
        Label(text, systemImage: systemImage)
            .foregroundStyle(room.inkFaint).font(.callout)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.vertical, 12).padding(.horizontal, 8)
    }
}

/// A fired-notification row with actions.
struct NotificationRow: View {
    @Environment(AppModel.self) private var model
    @Environment(\.room) private var room
    let notification: ReminderNotification

    var body: some View {
        HStack(spacing: 10) {
            AsyncCoverImage(url: notification.imageURL, cornerRadius: 6)
                .frame(width: 44, height: 44)
            VStack(alignment: .leading, spacing: 2) {
                Text(notification.name).font(.displayTitle(16)).foregroundStyle(room.ink).lineLimit(1)
                Text(notification.message ?? notification.subtitle)
                    .font(.system(size: 12)).foregroundStyle(room.inkMuted).lineLimit(1)
                MonoCaps(RemindersFormat.relative(notification.dueDate), size: 9)
            }
            Spacer(minLength: 8)
            if notification.state == .snoozed {
                MonoCaps("Snoozed", size: 9, color: room.accent)
            }
            Button { model.actNotification(id: notification.id, action: "play") } label: {
                Image(systemName: "play.fill").font(.system(size: 10, weight: .bold))
                    .foregroundStyle(room.base)
                    .frame(width: 26, height: 26)
                    .background(Circle().fill(room.accent))
            }.buttonStyle(PressableButtonStyle()).help("Play")
            Button { model.actNotification(id: notification.id, action: "queue") } label: {
                Image(systemName: "text.append")
            }.buttonStyle(.plain).help("Add to queue")
            Menu {
                Button("1 hour") { model.snoozeNotification(id: notification.id, for: 3600) }
                Button("4 hours") { model.snoozeNotification(id: notification.id, for: 4 * 3600) }
                Button("Tomorrow") { model.snoozeNotification(id: notification.id, for: 24 * 3600) }
            } label: {
                Image(systemName: "clock.arrow.circlepath")
            }.menuStyle(.button).buttonStyle(.plain).menuIndicator(.hidden).fixedSize().help("Snooze")
            Button { model.actNotification(id: notification.id, action: "dismiss") } label: {
                Image(systemName: "xmark")
            }.buttonStyle(.plain).foregroundStyle(room.inkMuted).help("Dismiss")
        }
        .foregroundStyle(room.inkMuted)
        .padding(.vertical, 6).padding(.horizontal, 10)
        .background(RoundedRectangle(cornerRadius: Theme.rowRadius, style: .continuous).fill(room.ink.opacity(0.04)))
    }
}

/// A scheduled-reminder row with a cancel action.
struct ReminderRow: View {
    @Environment(AppModel.self) private var model
    @Environment(\.room) private var room
    let reminder: Reminder

    var body: some View {
        HStack(spacing: 10) {
            AsyncCoverImage(url: reminder.imageURL, cornerRadius: 6)
                .frame(width: 36, height: 36)
            VStack(alignment: .leading, spacing: 2) {
                Text(reminder.name).font(.displayTitle(15)).foregroundStyle(room.ink).lineLimit(1)
                HStack(spacing: 6) {
                    MonoCaps(RemindersFormat.absolute(reminder.nextDueDate), size: 9)
                    if reminder.recurrence != .none {
                        Image(systemName: "repeat").font(.system(size: 9, weight: .bold)).foregroundStyle(room.inkFaint)
                        MonoCaps(reminder.recurrence.label, size: 9)
                    }
                }
            }
            Spacer(minLength: 8)
            Button("Cancel") { model.cancelReminder(id: reminder.id) }
                .buttonStyle(RoomButtonStyle())
        }
        .padding(.vertical, 6).padding(.horizontal, 10)
    }
}

/// Presented once on launch when reminders fired while the app was closed.
struct DueRemindersSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.room) private var room
    @Environment(\.dismiss) private var dismiss
    var onShowAll: () -> Void = {}

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            VStack(alignment: .leading, spacing: 6) {
                MonoCaps("Reminders", size: 10, color: room.accent)
                Text("You wanted to hear these").font(.displayHero(26)).foregroundStyle(room.ink)
            }

            ScrollView {
                LazyVStack(spacing: 6) {
                    ForEach(model.reminders.openNotifications) { NotificationRow(notification: $0) }
                }
            }
            .frame(maxHeight: 320)

            HStack {
                Button("Show all") { dismiss(); onShowAll() }
                    .buttonStyle(RoomButtonStyle())
                Spacer()
                Button("Done") { dismiss() }.buttonStyle(RoomButtonStyle(kind: .primary))
            }
        }
        .padding(24)
        .frame(width: 480)
        .background { ZStack { room.base; Grain() }.ignoresSafeArea() }
    }
}

enum RemindersFormat {
    static func relative(_ date: Date) -> String {
        let f = RelativeDateTimeFormatter()
        f.unitsStyle = .full
        return f.localizedString(for: date, relativeTo: Date())
    }

    static func absolute(_ date: Date) -> String {
        let f = DateFormatter()
        f.dateFormat = "EEE, MMM d · h:mm a"
        return f.string(from: date)
    }
}

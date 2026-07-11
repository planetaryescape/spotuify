import SwiftUI
import SpotuifyKit

/// The Notifications page: an Inbox of fired reminders (Play / Queue / Snooze /
/// Dismiss) plus the list of Scheduled reminders (cancel).
struct RemindersView: View {
    @Environment(AppModel.self) private var model

    var body: some View {
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
            .padding(16)
        }
        .background(.background)
        .navigationTitle("Notifications")
        .task { await model.reminders.loadAll() }
    }

    private func sectionHeader(_ title: String, count: Int) -> some View {
        HStack {
            Text(title).font(.title3.bold())
            if count > 0 {
                Text("\(count)").font(.caption.bold())
                    .padding(.horizontal, 7).padding(.vertical, 2)
                    .background(.tint, in: Capsule()).foregroundStyle(.white)
            }
            Spacer()
        }
        .padding(.vertical, 6)
        .background(.background)
    }

    private func emptyRow(_ text: String, systemImage: String) -> some View {
        Label(text, systemImage: systemImage)
            .foregroundStyle(.secondary).font(.callout)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.vertical, 12)
    }
}

/// A fired-notification row with actions.
struct NotificationRow: View {
    @Environment(AppModel.self) private var model
    let notification: ReminderNotification

    var body: some View {
        HStack(spacing: 10) {
            AsyncCoverImage(url: notification.imageURL, cornerRadius: 6)
                .frame(width: 44, height: 44)
            VStack(alignment: .leading, spacing: 2) {
                Text(notification.name).font(.system(size: 13, weight: .medium)).lineLimit(1)
                Text(notification.message ?? notification.subtitle)
                    .font(.caption).foregroundStyle(.secondary).lineLimit(1)
                Text(RemindersFormat.relative(notification.dueDate))
                    .font(.caption2).foregroundStyle(.tertiary)
            }
            Spacer(minLength: 8)
            if notification.state == .snoozed {
                Text("Snoozed").font(.caption2).foregroundStyle(.orange)
            }
            Button { model.actNotification(id: notification.id, action: "play") } label: {
                Image(systemName: "play.circle.fill").font(.title3)
            }.buttonStyle(.plain).help("Play")
            Button { model.actNotification(id: notification.id, action: "queue") } label: {
                Image(systemName: "text.append")
            }.buttonStyle(.plain).help("Add to queue")
            Menu {
                Button("1 hour") { model.snoozeNotification(id: notification.id, for: 3600) }
                Button("4 hours") { model.snoozeNotification(id: notification.id, for: 4 * 3600) }
                Button("Tomorrow") { model.snoozeNotification(id: notification.id, for: 24 * 3600) }
            } label: {
                Image(systemName: "clock.arrow.circlepath")
            }.menuStyle(.borderlessButton).fixedSize().help("Snooze")
            Button { model.actNotification(id: notification.id, action: "dismiss") } label: {
                Image(systemName: "xmark.circle")
            }.buttonStyle(.plain).foregroundStyle(.secondary).help("Dismiss")
        }
        .padding(.vertical, 4).padding(.horizontal, 8)
        .background(RoundedRectangle(cornerRadius: Theme.rowRadius).fill(.primary.opacity(0.04)))
    }
}

/// A scheduled-reminder row with a cancel action.
struct ReminderRow: View {
    @Environment(AppModel.self) private var model
    let reminder: Reminder

    var body: some View {
        HStack(spacing: 10) {
            AsyncCoverImage(url: reminder.imageURL, cornerRadius: 6)
                .frame(width: 36, height: 36)
            VStack(alignment: .leading, spacing: 2) {
                Text(reminder.name).font(.system(size: 13, weight: .medium)).lineLimit(1)
                HStack(spacing: 6) {
                    Text(RemindersFormat.absolute(reminder.nextDueDate))
                    if reminder.recurrence != .none {
                        Label(reminder.recurrence.label, systemImage: "repeat").labelStyle(.titleAndIcon)
                    }
                }
                .font(.caption2).foregroundStyle(.secondary)
            }
            Spacer(minLength: 8)
            Button("Cancel") { model.cancelReminder(id: reminder.id) }
                .buttonStyle(.borderless).font(.caption)
        }
        .padding(.vertical, 4).padding(.horizontal, 8)
    }
}

/// Presented once on launch when reminders fired while the app was closed.
struct DueRemindersSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    var onShowAll: () -> Void = {}

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            HStack {
                Image(systemName: "bell.badge.fill").foregroundStyle(.tint)
                Text("Reminders").font(.title2.bold())
                Spacer()
            }
            Text("You wanted to listen to these:")
                .font(.callout).foregroundStyle(.secondary)

            ScrollView {
                LazyVStack(spacing: 6) {
                    ForEach(model.reminders.openNotifications) { NotificationRow(notification: $0) }
                }
            }
            .frame(maxHeight: 320)

            HStack {
                Button("Show all") { dismiss(); onShowAll() }
                Spacer()
                Button("Done") { dismiss() }.buttonStyle(.borderedProminent)
            }
        }
        .padding(20)
        .frame(width: 480)
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

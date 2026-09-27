import SwiftUI
import SpotuifyKit

struct DevicesView: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            EditorialPageHeader("Devices", eyebrow: "Where it plays")
            if !model.canListDevices {
                EmptyState(
                    "Devices unavailable", systemImage: "hifispeaker.slash",
                    description: Text("The current provider does not expose playback devices."))
            } else if model.player.devices.isEmpty {
                EmptyState("No devices", systemImage: "hifispeaker",
                    description: Text("Open Spotify on another device to see it here."))
            } else {
                ScrollView {
                    LazyVGrid(columns: [GridItem(.adaptive(minimum: 240, maximum: 360), spacing: 16)], spacing: 16) {
                        ForEach(model.player.devices) { device in
                            DeviceRow(device: device)
                        }
                    }
                    .padding(.horizontal, 24).padding(.bottom, 24)
                }
            }
        }
    }
}

/// One device as a card: glyph, name in Fraunces, type and volume in the mono
/// voice. The live one carries the accent tick and says so in words, so the
/// state never rests on colour alone. Click to move playback here.
private struct DeviceRow: View {
    @Environment(AppModel.self) private var model
    @Environment(\.room) private var room
    let device: Device
    @State private var hovering = false

    var body: some View {
        Button {
            model.transfer(to: device)
        } label: {
            VStack(alignment: .leading, spacing: 14) {
                HStack(alignment: .top) {
                    Image(systemName: DeviceIcon.symbol(for: device.kind))
                        .font(.system(size: 22, weight: .light))
                        .foregroundStyle(device.isActive ? room.accent : room.inkMuted)
                        .frame(width: 44, height: 44)
                        .background(Circle().fill(room.ink.opacity(0.05)))
                        .overlay(Circle().strokeBorder(room.hairline))
                    Spacer()
                    if device.isActive {
                        HStack(spacing: 5) {
                            LevelMeter(isPlaying: model.player.isPlaying, size: 9)
                            MonoCaps("Playing here", size: 9, color: room.accent)
                        }
                        .foregroundStyle(room.accent)
                    }
                }
                VStack(alignment: .leading, spacing: 5) {
                    Text(device.name)
                        .font(.displayTitle(19))
                        .foregroundStyle(room.ink)
                        .lineLimit(1)
                    HStack(spacing: 8) {
                        MonoCaps(device.kind, size: 9.5)
                        if let volume = device.volumePercent {
                            MonoCaps("· Vol \(volume)%", size: 9.5)
                        }
                    }
                }
            }
            .padding(16)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background {
                RoundedRectangle(cornerRadius: Theme.tileCornerRadius, style: .continuous)
                    .fill(room.ink.opacity(device.isActive ? 0.07 : (hovering ? 0.06 : 0.04)))
            }
            .overlay {
                RoundedRectangle(cornerRadius: Theme.tileCornerRadius, style: .continuous)
                    .strokeBorder(device.isActive ? room.accent.opacity(0.5) : room.hairline)
            }
            .overlay(alignment: .leading) {
                // The accent tick, as in the sidebar: the live device.
                if device.isActive {
                    Capsule().fill(room.accent).frame(width: 3, height: 28).offset(x: -1)
                }
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .onHover { hovering = $0 }
        .disabled(!model.canTransferPlayback)
        .accessibilityLabel(device.isActive ? "\(device.name), playing here" : "Play on \(device.name)")
    }
}

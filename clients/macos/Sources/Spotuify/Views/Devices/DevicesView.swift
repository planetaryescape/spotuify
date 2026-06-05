import SwiftUI
import SpotuifyKit

struct DevicesView: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Text("Devices").font(.title2.bold()).padding(16)
            Divider()
            if model.player.devices.isEmpty {
                ContentUnavailableView("No devices", systemImage: "hifispeaker",
                    description: Text("Open Spotify on another device to see it here."))
            } else {
                ScrollView {
                    LazyVStack(spacing: 8) {
                        ForEach(model.player.devices) { device in
                            DeviceRow(device: device)
                        }
                    }
                    .padding(14)
                }
            }
        }
        .background(.background)
    }
}

private struct DeviceRow: View {
    @Environment(AppModel.self) private var model
    let device: Device
    @State private var hovering = false

    var body: some View {
        Button {
            model.transfer(to: device)
        } label: {
            HStack(spacing: 12) {
                Image(systemName: icon)
                    .font(.title2)
                    .frame(width: 32)
                    .foregroundStyle(device.isActive ? AnyShapeStyle(.tint) : AnyShapeStyle(.secondary))
                VStack(alignment: .leading, spacing: 2) {
                    Text(device.name)
                        .font(.system(size: 14, weight: device.isActive ? .semibold : .regular))
                    Text(device.isActive ? "Playing here" : device.kind)
                        .font(.caption)
                        .foregroundStyle(device.isActive ? AnyShapeStyle(.tint) : AnyShapeStyle(.secondary))
                }
                Spacer()
                if let volume = device.volumePercent {
                    Label("\(volume)%", systemImage: "speaker.wave.2")
                        .font(.caption).foregroundStyle(.secondary).labelStyle(.titleOnly)
                }
                if device.isActive {
                    Image(systemName: "checkmark.circle.fill").foregroundStyle(.tint)
                }
            }
            .padding(12)
            .background {
                RoundedRectangle(cornerRadius: 10)
                    .fill(device.isActive ? AnyShapeStyle(.tint.opacity(0.12))
                          : (hovering ? AnyShapeStyle(.primary.opacity(0.06)) : AnyShapeStyle(.quaternary.opacity(0.4))))
            }
        }
        .buttonStyle(.plain)
        .onHover { hovering = $0 }
    }

    private var icon: String {
        switch device.kind.lowercased() {
        case "computer": "laptopcomputer"
        case "smartphone": "iphone"
        case "speaker": "hifispeaker.fill"
        case "tv", "castvideo": "tv"
        case "avr", "stb": "av.remote"
        case "automobile": "car.fill"
        default: "hifispeaker"
        }
    }
}

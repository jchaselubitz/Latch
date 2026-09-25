import LatchMobileKit
import PhotosUI
import SwiftUI
import UIKit
import UniformTypeIdentifiers

/// What the composer can do with attachments. `add` is nil when the Mac does
/// not serve the attachments route or this device may not use it; the "+"
/// menu then leaves the three attachment items out entirely.
struct ConversationAttachmentControls {
    var items: [ConversationAttachment] = []
    var phase: ConversationAttachmentPhase = .idle
    var add: ((ConversationAttachment) -> Void)?
    var remove: (UUID) -> Void = { _ in }

    var isAvailable: Bool { add != nil }
    var isUploading: Bool { phase == .uploading }
}

/// Which picker the "+" menu asked for.
enum ConversationAttachmentSource: Identifiable {
    case photoLibrary
    case camera
    case file

    var id: Self { self }

    static var cameraAvailable: Bool {
        UIImagePickerController.isSourceTypeAvailable(.camera)
    }
}

/// Pending files as a row of chips above the field, each with a remove
/// control, and the reason a send with them failed.
struct ConversationAttachmentStrip: View {
    let controls: ConversationAttachmentControls

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            if !controls.items.isEmpty {
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack(spacing: 8) {
                        ForEach(controls.items) { item in
                            ConversationAttachmentChip(
                                attachment: item,
                                isUploading: controls.isUploading && item.receipt == nil,
                                remove: controls.isUploading ? nil : { controls.remove(item.id) }
                            )
                        }
                    }
                    // Room for the remove badge, which overhangs each chip.
                    .padding(.top, 6)
                    .padding(.trailing, 6)
                }
                .accessibilityIdentifier("conversation.composer.attachments")
            }
            if case .failed(let reason) = controls.phase {
                Label(reason, systemImage: "exclamationmark.triangle")
                    .font(.caption)
                    .foregroundStyle(.red)
                    .lineLimit(3)
                    .fixedSize(horizontal: false, vertical: true)
                    .accessibilityIdentifier("conversation.composer.attachmentError")
            }
        }
    }
}

private struct ConversationAttachmentChip: View {
    let attachment: ConversationAttachment
    let isUploading: Bool
    let remove: (() -> Void)?

    private let side: CGFloat = 56

    var body: some View {
        preview
            .frame(width: side, height: side)
            .clipShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
            .overlay {
                RoundedRectangle(cornerRadius: 12, style: .continuous)
                    .strokeBorder(Color(.separator), lineWidth: 0.5)
            }
            .overlay {
                if isUploading {
                    ZStack {
                        RoundedRectangle(cornerRadius: 12, style: .continuous)
                            .fill(.black.opacity(0.35))
                        ProgressView().tint(.white)
                    }
                }
            }
            .overlay(alignment: .topTrailing) {
                if let remove {
                    Button(action: remove) {
                        Image(systemName: "xmark.circle.fill")
                            .font(.system(size: 20))
                            .symbolRenderingMode(.palette)
                            .foregroundStyle(.white, Color(.systemGray))
                    }
                    .buttonStyle(.plain)
                    .offset(x: 6, y: -6)
                    .accessibilityLabel("Remove \(attachment.name)")
                }
            }
            .accessibilityElement(children: .contain)
            .accessibilityLabel(attachment.name)
    }

    @ViewBuilder
    private var preview: some View {
        if let thumbnail = attachment.thumbnail, let image = UIImage(data: thumbnail) {
            Image(uiImage: image)
                .resizable()
                .scaledToFill()
        } else {
            VStack(spacing: 2) {
                Image(systemName: attachment.kind == .image ? "photo" : "doc")
                    .font(.system(size: 18))
                Text(attachment.name)
                    .font(.system(size: 9))
                    .lineLimit(2)
                    .multilineTextAlignment(.center)
                    .padding(.horizontal, 3)
            }
            .foregroundStyle(.secondary)
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .background(Color(.secondarySystemBackground))
        }
    }
}

/// Presents the picker the "+" menu chose and turns what comes back into an
/// attachment. Photos become JPEG, which every agent can read; HEIC from the
/// library is not something an agent's image tool will open.
struct ConversationAttachmentPickers: ViewModifier {
    @Binding var source: ConversationAttachmentSource?
    let add: ((ConversationAttachment) -> Void)?

    @State private var photoSelection: [PhotosPickerItem] = []

    func body(content: Content) -> some View {
        content
            .photosPicker(
                isPresented: isPresented(.photoLibrary),
                selection: $photoSelection,
                maxSelectionCount: 4,
                matching: .images
            )
            .onChange(of: photoSelection) { _, items in
                guard !items.isEmpty else { return }
                photoSelection = []
                Task { await loadPhotos(items) }
            }
            .fullScreenCover(isPresented: isPresented(.camera)) {
                CameraCapture { image in
                    source = nil
                    if let image { deliver(image: image, name: "camera.jpg") }
                }
                .ignoresSafeArea()
            }
            .fileImporter(
                isPresented: isPresented(.file),
                allowedContentTypes: [.item],
                allowsMultipleSelection: true
            ) { result in
                guard case .success(let urls) = result else { return }
                urls.forEach(deliver(file:))
            }
    }

    private func isPresented(_ kind: ConversationAttachmentSource) -> Binding<Bool> {
        Binding(
            get: { source == kind },
            set: { if !$0, source == kind { source = nil } }
        )
    }

    @MainActor
    private func loadPhotos(_ items: [PhotosPickerItem]) async {
        for item in items {
            guard let data = try? await item.loadTransferable(type: Data.self),
                  let image = UIImage(data: data)
            else { continue }
            deliver(image: image, name: "photo.jpg")
        }
    }

    private func deliver(image: UIImage, name: String) {
        guard let add, let jpeg = AttachmentImageEncoding.jpeg(image) else { return }
        add(ConversationAttachment(
            name: name,
            data: jpeg,
            kind: .image,
            thumbnail: AttachmentImageEncoding.thumbnail(image)
        ))
    }

    private func deliver(file url: URL) {
        guard let add else { return }
        let scoped = url.startAccessingSecurityScopedResource()
        defer { if scoped { url.stopAccessingSecurityScopedResource() } }
        guard let data = try? Data(contentsOf: url) else { return }
        let isImage = UTType(filenameExtension: url.pathExtension)?.conforms(to: .image) == true
        let thumbnail = isImage ? UIImage(data: data).flatMap(AttachmentImageEncoding.thumbnail) : nil
        add(ConversationAttachment(
            name: url.lastPathComponent,
            data: data,
            kind: isImage ? .image : .file,
            thumbnail: thumbnail
        ))
    }
}

extension View {
    func conversationAttachmentPickers(
        source: Binding<ConversationAttachmentSource?>,
        add: ((ConversationAttachment) -> Void)?
    ) -> some View {
        modifier(ConversationAttachmentPickers(source: source, add: add))
    }
}

enum AttachmentImageEncoding {
    /// Long edge a photo is reduced to. Agents downscale larger images anyway,
    /// so sending more only costs upload time on a phone connection.
    static let maximumPixels: CGFloat = 2048

    static func jpeg(_ image: UIImage) -> Data? {
        let longest = max(image.size.width, image.size.height) * image.scale
        guard longest > maximumPixels else { return image.jpegData(compressionQuality: 0.85) }
        let factor = maximumPixels / longest
        let size = CGSize(
            width: image.size.width * image.scale * factor,
            height: image.size.height * image.scale * factor
        )
        let format = UIGraphicsImageRendererFormat()
        format.scale = 1
        let resized = UIGraphicsImageRenderer(size: size, format: format).image { _ in
            image.draw(in: CGRect(origin: .zero, size: size))
        }
        return resized.jpegData(compressionQuality: 0.85)
    }

    static func thumbnail(_ image: UIImage) -> Data? {
        image.preparingThumbnail(of: CGSize(width: 168, height: 168))?.jpegData(compressionQuality: 0.7)
    }
}

/// The system camera, for one still photo.
private struct CameraCapture: UIViewControllerRepresentable {
    let finish: (UIImage?) -> Void

    func makeCoordinator() -> Coordinator { Coordinator(finish: finish) }

    func makeUIViewController(context: Context) -> UIImagePickerController {
        let picker = UIImagePickerController()
        picker.sourceType = .camera
        picker.cameraCaptureMode = .photo
        picker.delegate = context.coordinator
        return picker
    }

    func updateUIViewController(_ controller: UIImagePickerController, context: Context) {}

    final class Coordinator: NSObject, UIImagePickerControllerDelegate, UINavigationControllerDelegate {
        let finish: (UIImage?) -> Void

        init(finish: @escaping (UIImage?) -> Void) { self.finish = finish }

        func imagePickerController(
            _ picker: UIImagePickerController,
            didFinishPickingMediaWithInfo info: [UIImagePickerController.InfoKey: Any]
        ) {
            finish(info[.originalImage] as? UIImage)
        }

        func imagePickerControllerDidCancel(_ picker: UIImagePickerController) {
            finish(nil)
        }
    }
}

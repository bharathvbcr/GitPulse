import Foundation
import UIKit

/// Native iOS Steering Confirmation Sheet (Opus #23 P1 steering authenticity).
///
/// Security model:
/// - Renders the exact UTF-8 steering text natively in UIKit (outside JS/OTA).
/// - Computes `text_sha256` directly over the rendered UTF-8 bytes.
/// - Builds `gitpulse-steering/v1\n` + canonical JSON signing bytes via Rust
///   (`GitPulseRustBridge.shared.buildSteeringSigningBytes`).
/// - Signs with the same non-exportable Secure Enclave P-256 key under `.biometryCurrentSet`
///   Face ID / Touch ID before returning `SignedSteeringConfirm`.
final class GitPulseSteeringSheetPresenter {
    static let shared = GitPulseSteeringSheetPresenter()

    func present(
        request: [String: Any],
        signer: @escaping (Data) throws -> Data,
        deviceIdProvider: @escaping () throws -> String,
        completion: @escaping ([String: Any]) -> Void
    ) {
        guard
            let hostId = request["host_id"] as? String,
            let sessionKey = request["session_key"] as? String,
            let requestId = request["request_id"] as? String,
            let expectRoute = request["expect_route"] as? String,
            let deliveryMode = request["delivery_mode"] as? String,
            let text = request["text"] as? String,
            !text.isEmpty,
            let repoLabel = request["repo_label"] as? String,
            let harnessLabel = request["harness_label"] as? String
        else {
            completion([
                "status": "error",
                "error_message": "Malformed steering confirmation sheet request",
            ])
            return
        }

        let textSha256: String
        do {
            textSha256 = try GitPulseRustBridge.shared.hashSteeringText(text: text)
        } catch {
            completion([
                "status": "error",
                "error_message": "Rust hash_steering_text failed: \(error.localizedDescription)",
            ])
            return
        }
        let ttlSec = Int64((request["ttl_sec"] as? NSNumber)?.int64Value ?? 300)

        guard let topVC = Self.topViewController() else {
            completion([
                "status": "cancelled",
                "error_message": "No active root view controller to present steering sheet",
            ])
            return
        }

        let sheetVC = NativeSteeringViewController(
            hostId: hostId,
            sessionKey: sessionKey,
            requestId: requestId,
            expectRoute: expectRoute,
            deliveryMode: deliveryMode,
            exactText: text,
            textSha256: textSha256,
            repoLabel: repoLabel,
            harnessLabel: harnessLabel,
            warning: request["warning"] as? String,
            ttlSec: max(30, min(ttlSec, 900)),
            signer: signer,
            deviceIdProvider: deviceIdProvider,
            completion: completion
        )
        sheetVC.modalPresentationStyle = .pageSheet
        if let sheet = sheetVC.sheetPresentationController {
            sheet.detents = [.large()]
            sheet.prefersGrabberVisible = true
        }
        topVC.present(sheetVC, animated: true)
    }

    private static func topViewController() -> UIViewController? {
        let scenes = UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }
        let root = scenes.first(where: { $0.activationState == .foregroundActive })?.windows.first(where: { $0.isKeyWindow })?.rootViewController
            ?? scenes.first?.windows.first?.rootViewController
        var current = root
        while let presented = current?.presentedViewController {
            current = presented
        }
        return current
    }
}

private final class NativeSteeringViewController: UIViewController {
    private let hostId: String
    private let sessionKey: String
    private let requestId: String
    private let expectRoute: String
    private let deliveryMode: String
    private let exactText: String
    private let textSha256: String
    private let repoLabel: String
    private let harnessLabel: String
    private let warning: String?
    private let ttlSec: Int64
    private let signer: (Data) throws -> Data
    private let deviceIdProvider: () throws -> String
    private let completion: ([String: Any]) -> Void
    private var completed = false

    init(
        hostId: String,
        sessionKey: String,
        requestId: String,
        expectRoute: String,
        deliveryMode: String,
        exactText: String,
        textSha256: String,
        repoLabel: String,
        harnessLabel: String,
        warning: String?,
        ttlSec: Int64,
        signer: @escaping (Data) throws -> Data,
        deviceIdProvider: @escaping () throws -> String,
        completion: @escaping ([String: Any]) -> Void
    ) {
        self.hostId = hostId
        self.sessionKey = sessionKey
        self.requestId = requestId
        self.expectRoute = expectRoute
        self.deliveryMode = deliveryMode
        self.exactText = exactText
        self.textSha256 = textSha256
        self.repoLabel = repoLabel
        self.harnessLabel = harnessLabel
        self.warning = warning
        self.ttlSec = ttlSec
        self.signer = signer
        self.deviceIdProvider = deviceIdProvider
        self.completion = completion
        super.init(nibName: nil, bundle: nil)
    }

    required init?(coder: NSCoder) { fatalError("init(coder:) not supported") }

    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = UIColor(red: 0.06, green: 0.09, blue: 0.16, alpha: 1.0)
        buildLayout()
    }

    override func viewDidDisappear(_ animated: Bool) {
        super.viewDidDisappear(animated)
        if !completed {
            completed = true
            completion(["status": "cancelled"])
        }
    }

    private func buildLayout() {
        let titleLabel = UILabel()
        titleLabel.text = "Hardware-Signed Steering Confirm"
        titleLabel.font = .boldSystemFont(ofSize: 20)
        titleLabel.textColor = .white

        let metaLabel = UILabel()
        metaLabel.numberOfLines = 0
        metaLabel.font = .monospacedSystemFont(ofSize: 12, weight: .medium)
        metaLabel.textColor = UIColor(white: 0.78, alpha: 1.0)
        var metaLines = [
            "Repo: \(repoLabel)",
            "Harness: \(harnessLabel)",
            "Session: \(sessionKey)",
            "Route: \(expectRoute) (\(deliveryMode))",
            "Request ID: \(requestId)",
            "Text SHA-256: \(textSha256)",
        ]
        if let warning = warning, !warning.isEmpty {
            metaLines.append("Warning: \(warning)")
        }
        metaLabel.text = metaLines.joined(separator: "\n")

        // Exact UTF-8 steering text rendered natively (Opus #23 P1).
        let textView = UITextView()
        textView.isEditable = false
        textView.backgroundColor = UIColor(red: 0.03, green: 0.05, blue: 0.10, alpha: 1.0)
        textView.textColor = UIColor(red: 0.88, green: 0.94, blue: 1.0, alpha: 1.0)
        textView.font = .monospacedSystemFont(ofSize: 13, weight: .regular)
        textView.layer.cornerRadius = 10
        textView.textContainerInset = UIEdgeInsets(top: 12, left: 12, bottom: 12, right: 12)
        textView.text = exactText

        let signBtn = makeButton(
            title: "Sign & Send Steering (Face ID / Secure Enclave)",
            color: UIColor(red: 0.09, green: 0.64, blue: 0.29, alpha: 1.0)
        ) { [weak self] in
            self?.signAndFinish()
        }
        let cancelBtn = makeButton(title: "Cancel", color: UIColor(white: 0.22, alpha: 1.0)) { [weak self] in
            self?.finish(["status": "cancelled"])
        }

        let stack = UIStackView(arrangedSubviews: [
            titleLabel,
            metaLabel,
            textView,
            signBtn,
            cancelBtn,
        ])
        stack.axis = .vertical
        stack.spacing = 12
        stack.translatesAutoresizingMaskIntoConstraints = false
        view.addSubview(stack)

        NSLayoutConstraint.activate([
            stack.topAnchor.constraint(equalTo: view.safeAreaLayoutGuide.topAnchor, constant: 16),
            stack.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 16),
            stack.trailingAnchor.constraint(equalTo: view.trailingAnchor, constant: -16),
            stack.bottomAnchor.constraint(equalTo: view.safeAreaLayoutGuide.bottomAnchor, constant: -16),
            textView.heightAnchor.constraint(greaterThanOrEqualToConstant: 180),
        ])
    }

    private func makeButton(title: String, color: UIColor, action: @escaping () -> Void) -> UIButton {
        let button = UIButton(type: .system)
        button.setTitle(title, for: .normal)
        button.setTitleColor(.white, for: .normal)
        button.titleLabel?.font = .boldSystemFont(ofSize: 15)
        button.backgroundColor = color
        button.layer.cornerRadius = 10
        button.heightAnchor.constraint(equalToConstant: 48).isActive = true
        button.addAction(UIAction { _ in action() }, for: .touchUpInside)
        return button
    }

    private func signAndFinish() {
        do {
            let deviceId = try deviceIdProvider()
            let nonce = UUID().uuidString.lowercased()
            let issuedAt = Int64(Date().timeIntervalSince1970)
            let expiresAt = issuedAt + ttlSec

            let signingBytes = try GitPulseRustBridge.shared.buildSteeringSigningBytes(
                hostId: hostId,
                sessionKey: sessionKey,
                requestId: requestId,
                expectRoute: expectRoute,
                textSha256: textSha256,
                nonce: nonce,
                issuedAt: issuedAt,
                expiresAt: expiresAt
            )
            // Triggers Face ID / Touch ID via Secure Enclave `.biometryCurrentSet`.
            let signatureDer = try signer(signingBytes)
            finish([
                "status": "signed",
                "computed_text_sha256": textSha256,
                "signed_confirm": [
                    "host_id": hostId,
                    "session_key": sessionKey,
                    "request_id": requestId,
                    "expect_route": expectRoute,
                    "text_sha256": textSha256,
                    "issued_at": issuedAt,
                    "expires_at": expiresAt,
                    "nonce": nonce,
                    "device_id": deviceId,
                    "sig_p256_der_b64": signatureDer.base64EncodedString(),
                ],
            ])
        } catch {
            finish([
                "status": "biometric_failed",
                "error_message": error.localizedDescription,
            ])
        }
    }

    private func finish(_ outcome: [String: Any]) {
        guard !completed else { return }
        completed = true
        dismiss(animated: true) {
            self.completion(outcome)
        }
    }
}

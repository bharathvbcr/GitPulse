import ExpoModulesCore
import Foundation
import UIKit

/// Native iOS Approval Confirmation Sheet (Opus M1 & M3).
/// Calls Rust `render_canonical_tool_input` and `hash_canonical` (board #18) so the exact
/// canonical JSON bytes rendered natively are identical to what is hashed and signed by Secure Enclave P-256.
final class WaylodeApprovalSheetPresenter {
    static let shared = WaylodeApprovalSheetPresenter()

    func present(
        request: [String: Any],
        signer: @escaping (Data) throws -> Data,
        deviceIdProvider: @escaping () throws -> String,
        completion: @escaping ([String: Any]) -> Void
    ) {
        guard
            let hostId = request["host_id"] as? String,
            let ticketId = request["ticket_id"] as? String,
            let toolName = request["tool_name"] as? String,
            let toolInputJson = request["tool_input_json"] as? String,
            let cwd = request["cwd"] as? String,
            let expectedSha = request["expected_input_sha256"] as? String,
            let repoLabel = request["repo_label"] as? String,
            let harnessLabel = request["harness_label"] as? String,
            let expiresAtSecs = (request["expires_at"] as? NSNumber)?.int64Value
        else {
            completion([
                "status": "cancelled",
                "error_message": "Malformed approval sheet request",
            ])
            return
        }

        let canonicalRendered: String
        let computedSha: String
        do {
            canonicalRendered = try WaylodeRustBridge.shared.renderCanonicalToolInput(
                toolName: toolName,
                toolInputJson: toolInputJson,
                cwd: cwd
            )
            computedSha = try WaylodeRustBridge.shared.hashCanonical(
                toolName: toolName,
                toolInputJson: toolInputJson,
                cwd: cwd
            )
        } catch {
            completion([
                "status": "hash_mismatch",
                "error_message": "Rust canonical render/hash failed: \(error.localizedDescription)",
            ])
            return
        }

        guard computedSha.lowercased() == expectedSha.lowercased() else {
            completion([
                "status": "hash_mismatch",
                "computed_input_sha256": computedSha,
                "error_message": "Canonical payload hash mismatch; refusing to sign.",
            ])
            return
        }

        guard let topVC = Self.topViewController() else {
            completion([
                "status": "cancelled",
                "error_message": "No active root view controller to present approval sheet",
            ])
            return
        }

        let sheetVC = NativeApprovalViewController(
            hostId: hostId,
            ticketId: ticketId,
            toolName: toolName,
            canonicalRendered: canonicalRendered,
            cwd: cwd,
            verifiedSha256: computedSha,
            repoLabel: repoLabel,
            worktreeLabel: request["worktree_label"] as? String,
            harnessLabel: harnessLabel,
            expiresAtSecs: expiresAtSecs,
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

private final class NativeApprovalViewController: UIViewController {
    private let hostId: String
    private let ticketId: String
    private let toolName: String
    private let canonicalRendered: String
    private let cwd: String
    private let verifiedSha256: String
    private let repoLabel: String
    private let worktreeLabel: String?
    private let harnessLabel: String
    private let expiresAtSecs: Int64
    private let signer: (Data) throws -> Data
    private let deviceIdProvider: () throws -> String
    private let completion: ([String: Any]) -> Void
    private var completed = false

    init(
        hostId: String,
        ticketId: String,
        toolName: String,
        canonicalRendered: String,
        cwd: String,
        verifiedSha256: String,
        repoLabel: String,
        worktreeLabel: String?,
        harnessLabel: String,
        expiresAtSecs: Int64,
        signer: @escaping (Data) throws -> Data,
        deviceIdProvider: @escaping () throws -> String,
        completion: @escaping ([String: Any]) -> Void
    ) {
        self.hostId = hostId
        self.ticketId = ticketId
        self.toolName = toolName
        self.canonicalRendered = canonicalRendered
        self.cwd = cwd
        self.verifiedSha256 = verifiedSha256
        self.repoLabel = repoLabel
        self.worktreeLabel = worktreeLabel
        self.harnessLabel = harnessLabel
        self.expiresAtSecs = expiresAtSecs
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
        titleLabel.text = "Hardware-Signed Approval"
        titleLabel.font = .boldSystemFont(ofSize: 20)
        titleLabel.textColor = .white

        let metaLabel = UILabel()
        metaLabel.numberOfLines = 0
        metaLabel.font = .monospacedSystemFont(ofSize: 12, weight: .medium)
        metaLabel.textColor = UIColor(white: 0.78, alpha: 1.0)
        let wtSuffix = worktreeLabel.map { " (\($0))" } ?? ""
        metaLabel.text = """
        Tool: \(toolName)
        Repo: \(repoLabel)\(wtSuffix)
        Harness: \(harnessLabel)
        CWD: \(cwd)
        Verified SHA-256: \(verifiedSha256)
        """

        let payloadView = UITextView()
        payloadView.isEditable = false
        payloadView.backgroundColor = UIColor(red: 0.03, green: 0.05, blue: 0.10, alpha: 1.0)
        payloadView.textColor = UIColor(red: 0.88, green: 0.94, blue: 1.0, alpha: 1.0)
        payloadView.font = .monospacedSystemFont(ofSize: 13, weight: .regular)
        payloadView.layer.cornerRadius = 10
        payloadView.textContainerInset = UIEdgeInsets(top: 12, left: 12, bottom: 12, right: 12)
        payloadView.text = canonicalRendered

        let allowBtn = makeButton(title: "Allow Once (Face ID / Secure Enclave)", color: UIColor(red: 0.09, green: 0.64, blue: 0.29, alpha: 1.0)) { [weak self] in
            self?.signAndFinish(decision: "allow_once")
        }
        let denyBtn = makeButton(title: "Deny", color: UIColor(red: 0.78, green: 0.22, blue: 0.22, alpha: 1.0)) { [weak self] in
            self?.signAndFinish(decision: "deny")
        }
        let denyStopBtn = makeButton(title: "Deny & Interrupt Session", color: UIColor(red: 0.55, green: 0.11, blue: 0.11, alpha: 1.0)) { [weak self] in
            self?.signAndFinish(decision: "deny_and_interrupt")
        }
        let cancelBtn = makeButton(title: "Cancel", color: UIColor(white: 0.22, alpha: 1.0)) { [weak self] in
            self?.finish(["status": "cancelled"])
        }

        let stack = UIStackView(arrangedSubviews: [
            titleLabel,
            metaLabel,
            payloadView,
            allowBtn,
            denyBtn,
            denyStopBtn,
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
            payloadView.heightAnchor.constraint(greaterThanOrEqualToConstant: 180),
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

    private func signAndFinish(decision: String) {
        guard decision == "allow_once" || decision == "deny" || decision == "deny_and_interrupt" else {
            finish(["status": "cancelled", "error_message": "Forbidden decision kind"])
            return
        }
        do {
            let deviceId = try deviceIdProvider()
            let nonce = UUID().uuidString.lowercased()
            let issuedAt = Int64(Date().timeIntervalSince1970)
            if issuedAt >= expiresAtSecs {
                finish(["status": "expired", "error_message": "Approval ticket expired"])
                return
            }

            let signingBytes = try WaylodeRustBridge.shared.buildApprovalSigningBytes(
                hostId: hostId,
                ticketId: ticketId,
                inputSha256: verifiedSha256,
                decision: decision,
                nonce: nonce,
                issuedAt: issuedAt,
                expiresAt: expiresAtSecs
            )
            let signatureDer = try signer(signingBytes)
            finish([
                "status": "signed",
                "computed_input_sha256": verifiedSha256,
                "signed_decision": [
                    "host_id": hostId,
                    "ticket_id": ticketId,
                    "input_sha256": verifiedSha256,
                    "decision": decision,
                    "nonce": nonce,
                    "issued_at": issuedAt,
                    "expires_at": expiresAtSecs,
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

final class WaylodeApprovalSheetView: ExpoView {
    let onOutcome = EventDispatcher()
    private let label = UILabel()

    required init(appContext: AppContext? = nil) {
        super.init(appContext: appContext)
        label.numberOfLines = 0
        label.font = .monospacedSystemFont(ofSize: 12, weight: .regular)
        label.textColor = .white
        addSubview(label)
    }

    override func layoutSubviews() {
        super.layoutSubviews()
        label.frame = bounds.insetBy(dx: 12, dy: 12)
    }

    func configure(request: [String: Any]) {
        let tool = request["tool_name"] as? String ?? "Unknown"
        let sha = request["expected_input_sha256"] as? String ?? ""
        label.text = "Native Approval Surface: \(tool)\nSHA-256: \(sha)"
    }
}

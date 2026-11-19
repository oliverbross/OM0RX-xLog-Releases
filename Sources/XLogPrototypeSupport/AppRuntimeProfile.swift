import Foundation
#if os(Windows)
import CWindowsOfflineProfile
#endif
#if canImport(Darwin)
import Darwin
#elseif canImport(Glibc)
import Glibc
#endif

public enum AppRuntimeProfile: Equatable, Sendable {
    case production
    case contestingTest(supportRoot: URL)

    static let contestingBundleIdentifier = "com.oliverbross.xlog.contesting-test"
    public static let contestingArgument = "--runtime-profile=contesting-test"
    public static let localAcceptanceArgument = "--local-ui-acceptance"
    public static let offlineAcceptancePrefix = "--offline-contest-acceptance="
    static let contestingIdentityPrefix = "com.oliverbross.OM0RXXLog.ContestingTest.Integration."

    private static let lock = NSLock()
    nonisolated(unsafe) private static var configured: AppRuntimeProfile?

    static var current: AppRuntimeProfile {
        lock.withLock { configured ?? .production }
    }

    public static func configure(_ profile: AppRuntimeProfile) throws {
        try lock.withLock {
            guard configured == nil else { throw WorkerFailure("runtime_profile_locked", "Runtime profile was already resolved.") }
            configured = profile
        }
    }

    public static func resolve(arguments: [String], executableURL: URL,
                        fileManager: FileManager = .default) throws -> AppRuntimeProfile {
        let profileArguments = arguments.filter { $0.hasPrefix("--runtime-profile=") }
        guard profileArguments.count <= 1 else { throw WorkerFailure("runtime_profile_invalid", "Runtime profile is invalid.") }
        let contents = executableURL.deletingLastPathComponent().deletingLastPathComponent()
        let infoURL = contents.appendingPathComponent("Info.plist")
        let bundleIdentifier: String? = fileManager.contents(atPath: infoURL.path).flatMap { data in
            (try? PropertyListSerialization.propertyList(from: data, format: nil) as? [String: Any])?["CFBundleIdentifier"] as? String
        }
        let testBundle = bundleIdentifier == contestingBundleIdentifier
        if arguments.contains(where: { $0.hasPrefix(offlineAcceptancePrefix) }) {
            #if os(Windows)
            let candidate = windowsPackageContainsWorker(executableURL, fileManager: fileManager)
            #else
            let candidate = bundleIdentifier == "com.oliverbross.xlog.integration"
            #endif
            guard arguments.count == 1, candidate else {
                throw WorkerFailure("runtime_profile_unbound", "Offline acceptance requires one production-candidate launch coordinate.")
            }
            let root = URL(fileURLWithPath: String(arguments[0].dropFirst(offlineAcceptancePrefix.count)), isDirectory: true)
            let resolved = try canonicalFilesystemPath(root)
            var temporaryParents = [fileManager.temporaryDirectory]
            #if !os(Windows)
            temporaryParents.append(URL(fileURLWithPath: "/tmp", isDirectory: true))
            #endif
            let parents = try temporaryParents.map { try canonicalFilesystemPath($0) }
            let attributes = try fileManager.attributesOfItem(atPath: root.path)
            let marker = root.appendingPathComponent(".xlog-offline-acceptance.json")
            let markerAttributes = try fileManager.attributesOfItem(atPath: marker.path)
            #if os(Windows)
            let privateStorage = windowsPrivatePath(root, directory: true) && windowsPrivatePath(marker, directory: false)
            #else
            let privateStorage = ((attributes[.posixPermissions] as? NSNumber)?.intValue ?? 0) & 0o077 == 0
            #endif
            guard root.path == resolved, root.lastPathComponent.hasPrefix("xlog-wave1-"),
                  parents.contains(root.deletingLastPathComponent().path),
                  attributes[.type] as? FileAttributeType == .typeDirectory,
                  privateStorage,
                  markerAttributes[.type] as? FileAttributeType == .typeRegular,
                  let bytes = fileManager.contents(atPath: marker.path), bytes.count <= 1024,
                  let value = try JSONSerialization.jsonObject(with: bytes) as? [String: Any], value["schema"] as? Int == 1,
                  let id = value["id"] as? String, UUID(uuidString: id) != nil else {
                throw WorkerFailure("runtime_profile_invalid", "Offline acceptance requires private marked disposable temporary storage.")
            }
            return .contestingTest(supportRoot: root.appendingPathComponent("worker", isDirectory: true))
        }
        guard profileArguments.first == contestingArgument else {
            if profileArguments.isEmpty, !testBundle { return .production }
            throw WorkerFailure("runtime_profile_invalid", "Runtime profile is invalid.")
        }
        guard testBundle else {
            throw WorkerFailure("runtime_profile_unbound", "Contesting Test profile is not bound to the expected application bundle.")
        }
        let applicationSupport = try fileManager.url(for: .applicationSupportDirectory,
            in: .userDomainMask, appropriateFor: nil, create: false)
        return try contestingTestProfile(applicationSupport: applicationSupport, fileManager: fileManager)
    }

    #if os(Windows)
    // Windows packages place the host, worker and generated provenance together.
    // This binds the profile to the package layout. Release digest verification
    // remains an independent step before launching that package.
    private static func windowsPackageContainsWorker(_ executable: URL, fileManager: FileManager) -> Bool {
        let directory = executable.deletingLastPathComponent()
        let host = directory.appendingPathComponent("xlog-prototype.exe")
        let manifest = directory.appendingPathComponent("provenance.json")
        guard executable.lastPathComponent == "xlog-prototype-worker.exe",
              fileManager.fileExists(atPath: executable.path), fileManager.fileExists(atPath: host.path),
              let size = try? manifest.resourceValues(forKeys: [.fileSizeKey]).fileSize, size <= 1_048_576,
              let data = try? Data(contentsOf: manifest),
              let metadata = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              metadata["schema"] as? Int == 1, metadata["product"] as? String == "OM0RX-xLog",
              metadata["platform"] as? String == "windows-x86_64",
              let payloads = metadata["payloads"] as? [[String: Any]] else { return false }
        let built = Set(payloads.filter { $0["origin"] as? String == "prototype-build" }.compactMap { $0["path"] as? String })
        return built.contains(host.lastPathComponent) && built.contains(executable.lastPathComponent)
    }

    private static func windowsPrivatePath(_ url: URL, directory: Bool) -> Bool {
        url.withUnsafeFileSystemRepresentation { native in
            guard let native else { return false }
            return String(cString: native).withCString(encodedAs: UTF16.self) {
                xlog_private_windows_path($0, directory ? 1 : 0) != 0
            }
        }
    }
    #endif

    /// Match Rust's filesystem canonicalization. Foundation deliberately presents
    /// /private/tmp and /private/var through their shorter macOS aliases.
    static func canonicalFilesystemPath(_ url: URL) throws -> String {
        #if canImport(Darwin) || canImport(Glibc)
        guard let path = realpath(url.path, nil) else {
            throw WorkerFailure("runtime_profile_invalid", "Offline acceptance directory is unavailable.")
        }
        defer { free(path) }
        return String(cString: path)
        #else
        return url.standardizedFileURL.resolvingSymlinksInPath().path
        #endif
    }

    /// Exact worker service modes. Unknown, duplicated, or reordered arguments
    /// remain fail-closed instead of silently acquiring acceptance authority.
    public static func workerServiceLocalAcceptance(arguments: [String]) -> Bool? {
        if arguments.count == 1, arguments[0].hasPrefix(offlineAcceptancePrefix) { return false }
        if arguments.isEmpty || arguments == [contestingArgument] { return false }
        if arguments == [localAcceptanceArgument]
            || arguments == [contestingArgument, localAcceptanceArgument] { return true }
        return nil
    }

    static func contestingTestProfile(applicationSupport: URL,
                                      fileManager: FileManager = .default) throws -> AppRuntimeProfile {
        let root = applicationSupport.appendingPathComponent("OM0RX-xLog Contesting Test", isDirectory: true)
        let production = [
            applicationSupport.appendingPathComponent("OM0RX-xLog", isDirectory: true),
            applicationSupport.appendingPathComponent("OM0RX-xLog-Integrated Preview", isDirectory: true),
        ]
        let resolvedRoot = root.standardizedFileURL.resolvingSymlinksInPath()
        guard resolvedRoot.path != applicationSupport.standardizedFileURL.resolvingSymlinksInPath().path,
              production.allSatisfy({ candidate in
                  let resolved = candidate.standardizedFileURL.resolvingSymlinksInPath()
                  return resolvedRoot.path != resolved.path && !resolvedRoot.path.hasPrefix(resolved.path + "/")
              }) else {
            throw WorkerFailure("runtime_profile_alias", "Contesting Test storage aliases production storage.")
        }
        return .contestingTest(supportRoot: root)
    }

    var externalIOAllowed: Bool {
        if case .production = self { return true }
        return false
    }

    var contestN1MMUDPAllowed: Bool {
        if case .production = self { return true }
        return connectedLoopbackAcceptance
    }

    /// Additional receive/loopback fixtures are admitted only by the already
    /// validated private offline root. This never enables station/output services.
    var connectedLoopbackAcceptance: Bool {
        guard case .contestingTest(let root)=self,root.lastPathComponent=="worker",
              root.deletingLastPathComponent().lastPathComponent.hasPrefix("xlog-wave1-"),
              let data=try? Data(contentsOf:root.deletingLastPathComponent().appendingPathComponent(".xlog-offline-acceptance.json")),data.count<=1024,
              let marker=try? JSONSerialization.jsonObject(with:data) as? [String:Any],marker["schema"] as? Int==1,
              marker["connectedLoopback"] as? Bool==true else{return false}
        return true
    }

    var supportDirectory: URL? {
        if case .contestingTest(let root) = self { return root }
        return nil
    }

    /// The resolved private support coordinate owns every offline ledger, too.
    /// Production retains its existing process-shared inventory location.
    func spotInventoryURL(applicationSupport: URL) -> URL {
        (supportDirectory ?? applicationSupport)
            .appendingPathComponent("OM0RX-xLog Integration Services", isDirectory: true)
            .appendingPathComponent("forwarded-radio-spots.json")
    }

    var identityPrefix: String {
        self == .production ? "com.oliverbross.OM0RXXLog.Integration." : Self.contestingIdentityPrefix
    }
}

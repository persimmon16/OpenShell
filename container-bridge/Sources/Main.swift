// SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

import ArgumentParser
import GRPC
import Logging
import NIOCore
import NIOPosix

/// Container Bridge daemon.
///
/// Translates gRPC calls from the gateway (running inside an Apple Container VM)
/// into XPC calls against the Apple Container runtime on the macOS host.
@main
struct ContainerBridgeDaemon: AsyncParsableCommand {
    static let configuration = CommandConfiguration(
        commandName: "container-bridge",
        abstract: "Bridge daemon translating gRPC to Apple Container XPC"
    )

    @Option(name: .long, help: "Port to listen on")
    var port: Int = 50052

    @Option(name: .long, help: "Host to bind to")
    var host: String = "127.0.0.1"

    func run() async throws {
        var logger = Logger(label: "com.openshell.container-bridge")
        logger.logLevel = .info

        let group = MultiThreadedEventLoopGroup(numberOfThreads: System.coreCount)
        defer {
            try? group.syncShutdownGracefully()
        }

        let provider = ContainerBridgeProvider(logger: logger)

        let server = try await Server.insecure(group: group)
            .withServiceProviders([provider])
            .bind(host: host, port: port)
            .get()

        logger.info("Container bridge listening on \(host):\(server.channel.localAddress!.port!)")

        // Block until the server shuts down.
        try await server.onClose.get()
    }
}

// Copyright (c) Microsoft Corporation. All rights reserved.
// Licensed under the MIT License.

//! Windows service support (behind the `windows-service` feature flag).

#[cfg(all(windows, feature = "windows-service"))]
pub mod win_svc {
    use std::ffi::OsString;
    use std::sync::mpsc;
    use std::time::Duration;

    use windows_service::service::{
        ServiceAccess, ServiceControl, ServiceControlAccept, ServiceErrorControl, ServiceExitCode,
        ServiceInfo, ServiceStartType, ServiceState, ServiceStatus, ServiceType,
    };
    use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
    use windows_service::service_dispatcher;
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};

    const SERVICE_NAME: &str = "FoundryACP";
    const DISPLAY_NAME: &str = "Foundry ACP Server";
    const DESCRIPTION: &str =
        "Agent Communication Protocol server for Foundry Local AI inference";

    const SERVICE_TYPE: ServiceType = ServiceType::OWN_PROCESS;

    /// Install the service into the Windows Service Control Manager.
    pub fn install_service(exe_path: &str) -> Result<(), Box<dyn std::error::Error>> {
        let manager =
            ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CREATE_SERVICE)?;

        let service_info = ServiceInfo {
            name: OsString::from(SERVICE_NAME),
            display_name: OsString::from(DISPLAY_NAME),
            service_type: SERVICE_TYPE,
            start_type: ServiceStartType::AutoStart,
            error_control: ServiceErrorControl::Normal,
            executable_path: std::path::PathBuf::from(exe_path),
            launch_arguments: vec![OsString::from("--run-as-service")],
            dependencies: vec![],
            account_name: None,
            account_password: None,
        };

        let service = manager.create_service(&service_info, ServiceAccess::CHANGE_CONFIG)?;
        service.set_description(DESCRIPTION)?;
        println!("Service '{}' installed successfully.", SERVICE_NAME);
        Ok(())
    }

    /// Uninstall the service.
    pub fn uninstall_service() -> Result<(), Box<dyn std::error::Error>> {
        let manager =
            ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)?;
        let service = manager.open_service(
            SERVICE_NAME,
            ServiceAccess::STOP | ServiceAccess::DELETE | ServiceAccess::QUERY_STATUS,
        )?;

        // Stop if running
        let status = service.query_status()?;
        if status.current_state != ServiceState::Stopped {
            service.stop()?;
            // Wait for stop
            for _ in 0..30 {
                std::thread::sleep(Duration::from_secs(1));
                let s = service.query_status()?;
                if s.current_state == ServiceState::Stopped {
                    break;
                }
            }
        }

        service.delete()?;
        println!("Service '{}' uninstalled successfully.", SERVICE_NAME);
        Ok(())
    }

    /// Entry point when running as a Windows service.
    pub fn run_as_service() -> Result<(), Box<dyn std::error::Error>> {
        service_dispatcher::start(SERVICE_NAME, ffi_service_main)?;
        Ok(())
    }

    extern "system" fn ffi_service_main(arguments: Vec<OsString>) {
        if let Err(e) = service_main(arguments) {
            eprintln!("Service error: {e}");
        }
    }

    fn service_main(_arguments: Vec<OsString>) -> Result<(), Box<dyn std::error::Error>> {
        let (shutdown_tx, shutdown_rx) = mpsc::channel::<()>();

        let event_handler = move |control_event| -> ServiceControlHandlerResult {
            match control_event {
                ServiceControl::Stop | ServiceControl::Shutdown => {
                    let _ = shutdown_tx.send(());
                    ServiceControlHandlerResult::NoError
                }
                ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
                _ => ServiceControlHandlerResult::NotImplemented,
            }
        };

        let status_handle =
            service_control_handler::register(SERVICE_NAME, event_handler)?;

        status_handle.set_service_status(ServiceStatus {
            service_type: SERVICE_TYPE,
            current_state: ServiceState::Running,
            controls_accepted: ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
            exit_code: ServiceExitCode::Win32(0),
            checkpoint: 0,
            wait_hint: Duration::default(),
            process_id: None,
        })?;

        // Start the server in a runtime
        let rt = tokio::runtime::Runtime::new()?;
        let handle = rt.spawn(async {
            if let Err(e) = crate::start_server("0.0.0.0", 8088, None).await {
                eprintln!("Server error: {e}");
            }
        });

        // Wait for stop signal
        let _ = shutdown_rx.recv();

        // Shut down
        handle.abort();

        status_handle.set_service_status(ServiceStatus {
            service_type: SERVICE_TYPE,
            current_state: ServiceState::Stopped,
            controls_accepted: ServiceControlAccept::empty(),
            exit_code: ServiceExitCode::Win32(0),
            checkpoint: 0,
            wait_hint: Duration::default(),
            process_id: None,
        })?;

        Ok(())
    }
}

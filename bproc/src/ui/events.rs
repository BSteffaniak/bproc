use super::*;
impl AppState {
    /// Promote painted component layouts only after TerminalPresenter commits.
    /// A failed/abandoned frame leaves the previous input scene authoritative.
    pub fn commit_component_scene(&mut self) {
        self.component_scene.borrow_mut().commit();
    }

    /// Dispatch once through the last successfully committed BMUX component
    /// scene. True means the runtime must not also execute a legacy alias.
    pub fn handle_component_event(
        &mut self,
        route: &bmux_tui::interaction::InteractionRoute,
    ) -> bool {
        let active = if self.show_backend_compatibility_dialog {
            "compatibility"
        } else if self.show_backend_info {
            "backend"
        } else if self.show_graph {
            "graph"
        } else if self.show_throttle_dialog {
            "throttle"
        } else if self.show_help {
            "help"
        } else {
            match self.view_mode {
                ViewMode::InterfaceList => "interfaces",
                ViewMode::ProcessDetail => match self.detail_tab {
                    ProcessDetailTab::Overview => "detail.overview",
                    ProcessDetailTab::Connections => "detail.connections",
                    ProcessDetailTab::Traffic => "detail.traffic",
                    ProcessDetailTab::System => "detail.system",
                },
                ViewMode::ProcessView => "draw_process_list",
                ViewMode::InterfaceDetail => "draw_interface_detail",
            }
        };
        let input_id = match self.throttle_dialog.selected_field {
            ThrottleField::Download => "throttle.download",
            ThrottleField::Upload => "throttle.upload",
        };
        let change = self
            .component_scene
            .borrow_mut()
            .event(route, active, input_id);
        match change {
            Some(scene::Change::Scroll(id, offset)) => {
                match id.as_str() {
                    "help" => self.help_scroll_offset = offset,
                    "backend" => self.backend_info_scroll_offset = offset,
                    "interfaces" => self.interface_modal_scroll_offset = offset,
                    "compatibility" => self.backend_compat_scroll_offset = offset,
                    _ if id.starts_with("detail.") => self.detail_scroll_offset = offset,
                    _ => {}
                }
                true
            }
            Some(scene::Change::Input(id, value)) => {
                if id == "throttle.download" {
                    self.throttle_dialog.download_input = value;
                    self.throttle_dialog.selected_field = ThrottleField::Download;
                } else {
                    self.throttle_dialog.upload_input = value;
                    self.throttle_dialog.selected_field = ThrottleField::Upload;
                }
                true
            }
            Some(scene::Change::Tab(index)) => {
                self.detail_tab = match index {
                    1 => ProcessDetailTab::Connections,
                    2 => ProcessDetailTab::Traffic,
                    3 => ProcessDetailTab::System,
                    _ => ProcessDetailTab::Overview,
                };
                self.detail_scroll_offset = 0;
                true
            }
            Some(scene::Change::List(id, selected, offset)) => {
                if id == "draw_interface_list" {
                    self.interface_list_state.select(selected);
                    *self.interface_list_state.offset_mut() = offset;
                    self.selected_interface_index = selected;
                } else {
                    self.list_state.select(selected);
                    *self.list_state.offset_mut() = offset;
                    self.selected_index = selected;
                }
                true
            }
            Some(scene::Change::Consumed) => true,
            None => false,
        }
    }
}

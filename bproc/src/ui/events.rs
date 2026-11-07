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
        let active = match self.top_layer() {
            Some(Layer::Compatibility) => "compatibility",
            Some(Layer::Backends) => "backend",
            Some(Layer::Graph) => "graph",
            Some(Layer::Throttle) => "throttle",
            Some(Layer::Help) => "help",
            Some(Layer::InterfaceList) => "interfaces",
            Some(Layer::InterfaceDetail) => "draw_interface_detail",
            None => return false,
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
            Some(scene::Change::List(selected, offset)) => {
                // Only the interface detail list is interactive.
                self.interface_detail_state.select(selected);
                *self.interface_detail_state.offset_mut() = offset;
                true
            }
            Some(scene::Change::Consumed) => true,
            None => false,
        }
    }
}

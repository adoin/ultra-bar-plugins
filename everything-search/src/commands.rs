use std::sync::mpsc::{Receiver, RecvError};
use ultra_bar_plugin_ui::Command;
#[derive(Default)]
pub struct Commands {
    pending: Option<Command>,
}
impl Commands {
    pub fn next(&mut self, input: &Receiver<Command>) -> Result<Command, RecvError> {
        let command = match self.pending.take() {
            Some(command) => command,
            None => input.recv()?,
        };
        Ok(self.coalesce(input, command))
    }
    pub fn next_timeout(
        &mut self,
        input: &Receiver<Command>,
        timeout: std::time::Duration,
    ) -> Result<Command, std::sync::mpsc::RecvTimeoutError> {
        let command = match self.pending.take() {
            Some(command) => command,
            None => input.recv_timeout(timeout)?,
        };
        Ok(self.coalesce(input, command))
    }
    fn coalesce(&mut self, input: &Receiver<Command>, command: Command) -> Command {
        let Command::Query(mut query) = command else {
            return command;
        };
        while let Ok(next) = input.try_recv() {
            match next {
                Command::Query(text) => query = text,
                other => {
                    self.pending = Some(other);
                    break;
                }
            }
        }
        Command::Query(query)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn newest_query_wins_without_discarding_refresh_or_settings_commands() {
        let (send, input) = std::sync::mpsc::channel();
        for query in ["i", "im", "ima", "imag", "image"] {
            send.send(Command::Query(query.into())).unwrap();
        }
        send.send(Command::Refresh).unwrap();
        send.send(Command::SettingsSaved(vec!["keep".into()]))
            .unwrap();
        send.send(Command::Query("photo".into())).unwrap();
        let mut queue = Commands::default();
        assert!(matches!(queue.next(&input).unwrap(), Command::Query(text) if text == "image"));
        assert!(matches!(queue.next(&input).unwrap(), Command::Refresh));
        assert!(
            matches!(queue.next(&input).unwrap(), Command::SettingsSaved(values) if values == ["keep"])
        );
        assert!(matches!(queue.next(&input).unwrap(), Command::Query(text) if text == "photo"));
    }
}

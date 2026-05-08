use pocketflow_rs::ProcessState;

#[derive(Debug, Clone, PartialEq)]
pub enum MyState {
    Success,
    Failure,
    Default,
}

impl ProcessState for MyState {
    fn is_default(&self) -> bool {
        matches!(self, MyState::Default)
    }

    fn to_condition(&self) -> String {
        match self {
            MyState::Success => "success".to_string(),
            MyState::Failure => "failure".to_string(),
            MyState::Default => "default".to_string(),
        }
    }
}

impl Default for MyState {
    fn default() -> Self {
        MyState::Default
    }
}

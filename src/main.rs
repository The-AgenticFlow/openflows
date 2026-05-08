mod state;
mod utils;
mod nodes;

use anyhow::Result;
use pocketflow_rs::{build_flow, Context};
use state::MyState;
use nodes::{GetQuestionNode, AnswerNode};

#[tokio::main]
async fn main() -> Result<()> {
    // Instantiate nodes
    let get_question = GetQuestionNode;
    let answer = AnswerNode;

    // Build flow
    let flow = build_flow!(
        start: ("get_question", get_question),
        nodes: [("answer", answer)],
        edges: [
            ("get_question", "answer", MyState::Success)
        ]
    );

    // Shared context
    let context = Context::new();

    // Run flow
    let result_context = flow.run(context).await?;

    // Read and print results
    if let Some(q) = result_context.get("question").and_then(|v| v.as_str()) {
        println!("Question: {}", q);
    }
    if let Some(a) = result_context.get("answer").and_then(|v| v.as_str()) {
        println!("Answer: {}", a);
    }

    Ok(())
}

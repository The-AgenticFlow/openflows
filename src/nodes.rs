use anyhow::Result;
use async_trait::async_trait;
use pocketflow_rs::{Context, Node, ProcessResult};
use serde_json::json;
use tokio::io::{self, AsyncBufReadExt};

use crate::state::MyState;
use crate::utils::call_llm;

pub struct GetQuestionNode;

#[async_trait]
impl Node for GetQuestionNode {
    type State = MyState;

    async fn execute(&self, _context: &Context) -> Result<serde_json::Value> {
        println!("Enter your question: ");
        let mut reader = io::BufReader::new(tokio::io::stdin());
        let mut line = String::new();
        reader.read_line(&mut line).await?;
        let question = line.trim().to_string();
        Ok(json!(question))
    }

    async fn post_process(
        &self,
        context: &mut Context,
        result: &Result<serde_json::Value>,
    ) -> Result<ProcessResult<MyState>> {
        if let Ok(val) = result {
            context.set("question", val.clone());
            Ok(ProcessResult::new(MyState::Success, "success".to_string()))
        } else {
            Ok(ProcessResult::new(MyState::Failure, "failure".to_string()))
        }
    }
}

pub struct AnswerNode;

#[async_trait]
impl Node for AnswerNode {
    type State = MyState;

    async fn execute(&self, context: &Context) -> Result<serde_json::Value> {
        let question = context
            .get("question")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let answer = call_llm(&question).await?;
        Ok(json!(answer))
    }

    async fn post_process(
        &self,
        context: &mut Context,
        result: &Result<serde_json::Value>,
    ) -> Result<ProcessResult<MyState>> {
        if let Ok(val) = result {
            context.set("answer", val.clone());
            Ok(ProcessResult::new(MyState::Success, "success".to_string()))
        } else {
            Ok(ProcessResult::new(MyState::Failure, "failure".to_string()))
        }
    }
}

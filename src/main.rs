mod config;
mod embeddings;
mod memory;
mod provider;

use std::io::{self, Write};
use std::path::Path;

use synaptic::core::Message;
use synaptic::graph::{create_react_agent, MessageState};

use memory::format_context;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let cfg = config::load(Path::new("config.toml"));

    let emb = embeddings::build_embeddings(&cfg.embeddings)?;
    let mem = memory::ConversationMemory::new(&cfg.memory, emb, cfg.embeddings.dimensions).await?;

    // Handle --import <file> --user <user_id> mode.
    let args: Vec<String> = std::env::args().collect();
    if args.len() >= 3 && args[1] == "--import" {
        let file_path = Path::new(&args[2]);
        let user_id = if args.len() >= 5 && args[3] == "--user" {
            &args[4]
        } else {
            &cfg.user.id
        };
        println!("Importing from {} for user '{user_id}'...", file_path.display());
        let count = mem.import_jsonl(file_path, user_id).await?;
        println!("Imported {count} entries.");
        return Ok(());
    }

    let model = provider::build_model(&cfg.llm)?;
    let graph = create_react_agent(model, vec![])?;

    println!(
        "Sidekick Agent ready (provider: {}, model: {}, user: {}).\nType 'quit' to exit.\n",
        cfg.llm.provider, cfg.llm.model, cfg.user.id
    );

    let user_id = &cfg.user.id;

    loop {
        print!("> ");
        io::stdout().flush().unwrap();

        let mut input = String::new();
        io::stdin().read_line(&mut input).unwrap();
        let input = input.trim();

        if input.eq_ignore_ascii_case("quit") || input.eq_ignore_ascii_case("exit") {
            break;
        }

        if input.is_empty() {
            continue;
        }

        // Retrieve relevant past entries from memory (all categories).
        let context_entries = mem.retrieve(user_id, input, None).await?;
        let context = format_context(&context_entries);

        // Build the system prompt: base persona + RAG context.
        let system_prompt = if context.is_empty() {
            cfg.agent.system_prompt.clone()
        } else {
            format!("{}\n\n{}", cfg.agent.system_prompt, context)
        };

        // Build the message list for this turn.
        let messages = vec![
            Message::system(&system_prompt),
            Message::human(input),
        ];

        let state = MessageState { messages };
        let result = graph.invoke(state).await?;
        let final_state = result.state();

        if let Some(msg) = final_state.last_message() {
            let response = msg.content();
            println!("\n{}\n", response);

            // Persist both the user message and the assistant response.
            mem.store(user_id, "conversation", "human", input).await?;
            mem.store(user_id, "conversation", "ai", response).await?;
        }
    }

    Ok(())
}

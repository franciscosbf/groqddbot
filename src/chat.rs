use std::{collections::VecDeque, iter::once, sync::Arc};

use genai::{
    chat::{ChatMessage, ChatRequest},
    resolver::AuthData,
};

type Response = String;

#[derive(Debug)]
struct User {
    client: genai::Client,
    model: Arc<String>,
}

impl User {
    fn new(key: String, model: Arc<String>) -> Self {
        Self {
            client: genai::Client::builder()
                .with_auth_resolver_fn(|_| Ok(Some(AuthData::from_single(key))))
                .build(),
            model,
        }
    }

    async fn send_message(&self, request: ChatRequest) -> Result<Response, genai::Error> {
        self.client
            .exec_chat(&self.model, request, None)
            .await
            .map(|cr| cr.content.unwrap().text_into_string().unwrap())
    }
}

#[derive(Debug)]
struct Interaction {
    user_message: ChatMessage,
    assistant_message: ChatMessage,
}

#[derive(Debug)]
pub struct Session {
    user: User,
    history: VecDeque<Interaction>,
}

impl Session {
    fn new(user: User, history_size: usize) -> Self {
        Self {
            user,
            history: VecDeque::with_capacity(history_size),
        }
    }

    fn append_to_history(&mut self, interaction: Interaction) {
        if self.history.len() == self.history.capacity() {
            self.history.pop_front();
        }

        self.history.push_back(interaction);
    }

    pub async fn send_message(&mut self, content: String) -> Result<Response, genai::Error> {
        let user_message = ChatMessage::user(content);

        let mut chat_request = ChatRequest::default();
        chat_request.messages.reserve_exact(self.history.len() + 1);
        let history = self
            .history
            .iter()
            .flat_map(|p| once(&p.user_message).chain(once(&p.assistant_message)))
            .cloned();
        chat_request.messages.extend(history);
        chat_request.messages.push(user_message.clone());

        let response = self.user.send_message(chat_request).await?;
        let assistant_message = ChatMessage::assistant(response.clone());

        self.append_to_history(Interaction {
            user_message,
            assistant_message,
        });

        Ok(response)
    }

    pub fn pop_last_interaction(&mut self) {
        self.history.pop_back();
    }
}

pub struct SessionBuilder {
    key: String,
    model: Arc<String>,
    history_size: usize,
}

impl SessionBuilder {
    pub fn new(key: String, model: String, history_size: usize) -> Self {
        Self {
            key,
            model: Arc::new(model),
            history_size,
        }
    }

    pub fn create_chat(&self) -> Session {
        let user = User::new(self.key.clone(), self.model.clone());

        Session::new(user, self.history_size)
    }
}

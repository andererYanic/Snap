use crate::database::FileRecord;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
pub enum Request {
    Publish { path: String },
    Download { 
        ticket: Option<String>,
        hash: Option<String>,
        output: Option<String>
    },
    Delete {
        id: Option<String>,
        hash: Option<String>,
        path: Option<String>,
    },
    List,
}

#[derive(Serialize, Deserialize, Debug)]
pub enum Response {
    Success { message: String },
    Ticket { ticket: String },
    FileList { files: Vec<FileRecord> },
    Error { message: String },
}

use std::collections::BTreeMap;

use super::Span;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceFile {
    pub span: Span,
    pub records: Vec<RecordAst>,
    pub definition: DefinitionAst,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DefinitionAst {
    Workflow(WorkflowAst),
    Agent(AgentAst),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecordAst {
    pub name: String,
    pub fields: Vec<FieldAst>,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FieldAst {
    pub name: String,
    pub ty: TypeAst,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkflowAst {
    pub name: String,
    pub inputs: Vec<FieldAst>,
    pub result: TypeAst,
    pub entry: CallAst,
    pub limits: BTreeMap<String, u64>,
    pub tasks: Vec<TaskAst>,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentAst {
    pub name: String,
    pub inputs: Vec<FieldAst>,
    pub result: TypeAst,
    pub entry: CallAst,
    pub events: Vec<EventDeclarationAst>,
    pub limits: BTreeMap<String, u64>,
    pub tasks: Vec<TaskAst>,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EventDeclarationAst {
    pub event_type: String,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaskAst {
    pub name: String,
    pub inputs: Vec<FieldAst>,
    pub result: TypeAst,
    pub model_role: Option<ModelRoleAst>,
    pub prompt: PromptAst,
    pub tools: Vec<String>,
    pub outcomes: Vec<OutcomeAst>,
    pub span: Span,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModelRoleAst {
    Reasoning,
    Coding,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PromptAst {
    pub text: String,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OutcomeAst {
    pub name: String,
    pub fields: Vec<FieldAst>,
    pub statements: Vec<StatementAst>,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CallAst {
    pub binding: Option<String>,
    pub task: String,
    pub arguments: Vec<ArgumentAst>,
    pub context: ContextModeAst,
    pub span: Span,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContextModeAst {
    Legacy,
    Isolated,
    Kept,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArgumentAst {
    pub name: String,
    pub value: ValueRefAst,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StatementAst {
    Call(CallAst),
    Parallel(Vec<CallAst>),
    Ask {
        binding: String,
        answer_type: TypeAst,
        question: ValueRefAst,
    },
    Goto(CallAst),
    Done(ValueRefAst),
    Fail {
        code: ValueRefAst,
        message: ValueRefAst,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TypeAst {
    String,
    Integer,
    Number,
    Boolean,
    Named(String),
    Optional(Box<TypeAst>),
    List(Box<TypeAst>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ValueRefAst {
    Binding(String),
    String(String),
    Number(String),
    Boolean(bool),
    Null,
}

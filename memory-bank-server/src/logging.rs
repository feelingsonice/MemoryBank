use rmcp::model::{LoggingLevel, LoggingMessageNotificationParam};
use tokio::sync::broadcast;
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Subscriber};
use tracing_subscriber::fmt::FormattedFields;
use tracing_subscriber::fmt::format::{DefaultFields, FormatFields, Writer};
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::{EnvFilter, Layer, fmt, prelude::*};

use crate::error::AppError;

pub struct Logging {
    sender: broadcast::Sender<LoggingMessageNotificationParam>,
}

impl Logging {
    pub fn initialize() -> Result<Self, AppError> {
        let (sender, _) = broadcast::channel(512);

        tracing_subscriber::registry()
            .with(fmt::layer().compact().with_writer(std::io::stderr))
            .with(McpLoggerLayer::new(sender.clone()))
            .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
            .try_init()?;

        Ok(Self { sender })
    }

    pub fn sender(&self) -> broadcast::Sender<LoggingMessageNotificationParam> {
        self.sender.clone()
    }
}

pub struct McpLoggerLayer {
    sender: broadcast::Sender<LoggingMessageNotificationParam>,
    fields: DefaultFields,
}

struct McpFormattedFields(FormattedFields<DefaultFields>);

impl McpLoggerLayer {
    pub fn new(sender: broadcast::Sender<LoggingMessageNotificationParam>) -> Self {
        Self {
            sender,
            fields: DefaultFields::new(),
        }
    }

    fn notification_for<S>(
        &self,
        event: &Event<'_>,
        ctx: Context<'_, S>,
    ) -> LoggingMessageNotificationParam
    where
        S: Subscriber + for<'a> LookupSpan<'a>,
    {
        LoggingMessageNotificationParam {
            level: match *event.metadata().level() {
                tracing::Level::TRACE | tracing::Level::DEBUG => LoggingLevel::Debug,
                tracing::Level::INFO => LoggingLevel::Info,
                tracing::Level::WARN => LoggingLevel::Warning,
                tracing::Level::ERROR => LoggingLevel::Error,
            },
            logger: Some(event.metadata().target().to_string()),
            data: serde_json::Value::String(self.format_event(event, ctx)),
        }
    }

    fn format_event<S>(&self, event: &Event<'_>, ctx: Context<'_, S>) -> String
    where
        S: Subscriber + for<'a> LookupSpan<'a>,
    {
        let scope = self.format_scope(event, ctx);
        let mut fields = String::new();

        if self
            .fields
            .format_fields(Writer::new(&mut fields), event)
            .is_ok()
            && !fields.is_empty()
        {
            if scope.is_empty() {
                fields
            } else {
                format!("{scope}: {fields}")
            }
        } else if !scope.is_empty() {
            scope
        } else {
            event.metadata().name().to_string()
        }
    }

    fn format_scope<S>(&self, event: &Event<'_>, ctx: Context<'_, S>) -> String
    where
        S: Subscriber + for<'a> LookupSpan<'a>,
    {
        let mut rendered_scope = Vec::new();

        if let Some(scope) = ctx.event_scope(event) {
            for span in scope.from_root() {
                let mut rendered = span.metadata().name().to_string();
                let extensions = span.extensions();
                if let Some(fields) = extensions.get::<McpFormattedFields>()
                    && !fields.0.is_empty()
                {
                    rendered.push('{');
                    rendered.push_str(&fields.0);
                    rendered.push('}');
                }
                rendered_scope.push(rendered);
            }
        }

        rendered_scope.join(": ")
    }
}

impl<S> Layer<S> for McpLoggerLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let span = ctx.span(id).expect("span should exist");
        let mut extensions = span.extensions_mut();

        if extensions.get_mut::<McpFormattedFields>().is_none() {
            let mut fields = FormattedFields::<DefaultFields>::new(String::new());
            if self.fields.format_fields(fields.as_writer(), attrs).is_ok() {
                extensions.insert(McpFormattedFields(fields));
            }
        }
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, ctx: Context<'_, S>) {
        let span = ctx.span(id).expect("span should exist");
        let mut extensions = span.extensions_mut();

        if let Some(fields) = extensions.get_mut::<McpFormattedFields>() {
            let _ = self.fields.add_fields(&mut fields.0, values);
            return;
        }

        let mut fields = FormattedFields::<DefaultFields>::new(String::new());
        if self
            .fields
            .format_fields(fields.as_writer(), values)
            .is_ok()
        {
            extensions.insert(McpFormattedFields(fields));
        }
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        let _ = self.sender.send(self.notification_for(event, ctx));
    }
}

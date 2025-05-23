use std::sync::Arc;

use polars_core::prelude::IntoColumn;
use polars_core::schema::Schema;

use super::compute_node_prelude::*;
use crate::expression::StreamExpr;

pub struct SelectNode {
    selectors: Vec<StreamExpr>,
    schema: Arc<Schema>,
    extend_original: bool,
}

impl SelectNode {
    pub fn new(selectors: Vec<StreamExpr>, schema: Arc<Schema>, extend_original: bool) -> Self {
        Self {
            selectors,
            schema,
            extend_original,
        }
    }
}

impl ComputeNode for SelectNode {
    fn name(&self) -> &str {
        if self.extend_original {
            "with-columns"
        } else {
            "select"
        }
    }

    fn update_state(
        &mut self,
        recv: &mut [PortState],
        send: &mut [PortState],
        _state: &StreamingExecutionState,
    ) -> PolarsResult<()> {
        assert!(recv.len() == 1 && send.len() == 1);
        recv.swap_with_slice(send);
        Ok(())
    }

    fn spawn<'env, 's>(
        &'env mut self,
        scope: &'s TaskScope<'s, 'env>,
        recv_ports: &mut [Option<RecvPort<'_>>],
        send_ports: &mut [Option<SendPort<'_>>],
        state: &'s StreamingExecutionState,
        join_handles: &mut Vec<JoinHandle<PolarsResult<()>>>,
    ) {
        assert!(recv_ports.len() == 1 && send_ports.len() == 1);
        let receivers = recv_ports[0].take().unwrap().parallel();
        let senders = send_ports[0].take().unwrap().parallel();

        for (mut recv, mut send) in receivers.into_iter().zip(senders) {
            let slf = &*self;
            join_handles.push(scope.spawn_task(TaskPriority::High, async move {
                while let Ok(morsel) = recv.recv().await {
                    let (df, seq, source_token, consume_token) = morsel.into_inner();
                    let mut selected = Vec::new();
                    for selector in slf.selectors.iter() {
                        let s = selector.evaluate(&df, &state.in_memory_exec_state).await?;
                        selected.push(s.into_column());
                    }

                    let ret = if slf.extend_original {
                        let mut out = df;
                        out._add_columns(selected, &slf.schema)?;
                        out
                    } else {
                        DataFrame::new_with_broadcast(selected)?
                    };

                    let mut morsel = Morsel::new(ret, seq, source_token);
                    if let Some(token) = consume_token {
                        morsel.set_consume_token(token);
                    }

                    if send.send(morsel).await.is_err() {
                        break;
                    }
                }

                Ok(())
            }));
        }
    }
}

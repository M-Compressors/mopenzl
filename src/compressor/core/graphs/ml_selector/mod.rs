//! The ML selector graph (`ZL_GRAPH_ML_SELECTOR`, graph 17): routes its
//! numeric input to one of its custom successors, chosen by a gradient
//! boosted tree model evaluated on statistics of the input.
//!
//! C sources: `compress/selectors/ml/ml_selector_graph.c`
//! (`ZL_MLSel_dynGraph`, `ZL_MLSel_materialize`), `gbt.c`, `features.c`.
//!
//! The model travels as a CBOR blob, the graph's MParam. It is decoded once,
//! when the graph is registered ([`deserialize`], the reference's
//! `ZL_MLSel_materialize`), and kept as a reference parameter under
//! [`MPARAM_REF_ID`](super::super::exec::MPARAM_REF_ID); a graph without it
//! fails like the reference's "config was not materialized".
//!
//! ```text
//! dynGraph: GBT model? -> successors == nbSuccessors? -> for each input:
//!   features = FeatureGen_integer(input)
//!   class    = GBTPredictor_predict(features)      (error if >= nbSuccessors)
//!   set_destination(input, customGraphs[class])
//! ```

mod config;
mod features;
mod gbt;

pub(crate) use config::{Config, ConfigError, deserialize, serialize};
pub(crate) use features::FeatureGen;
pub(crate) use gbt::{Model, Node};

use super::super::data::Type;
use super::super::error::ExecError;
use super::super::exec::{Edge, GraphCx};
use super::super::registry::GraphFn;

/// `ZL_MLSel_dynGraph`.
#[derive(Debug)]
pub(crate) struct MlSelector;

/// See [`MlSelector`].
pub(crate) static ML_SELECTOR: MlSelector = MlSelector;

impl GraphFn for MlSelector {
    fn run(&self, cx: &mut GraphCx<'_, '_, '_>, inputs: &[Edge]) -> Result<(), ExecError> {
        let config = cx
            .mparam()
            .and_then(|object| object.downcast_ref::<Config>())
            .ok_or(ExecError::Parameter(
                "ML selector config was not materialized",
            ))?;
        let Config::Gbt(model) = config else {
            return Err(ExecError::GraphInvalid);
        };
        let successors = cx.eff.custom_graphs;
        if model.nb_successors != successors.len() {
            return Err(ExecError::Parameter(
                "GBT model and graph have a different number of successors",
            ));
        }
        for &input in inputs {
            let selected = {
                let stream = cx.data(input)?;
                if stream.ty() != Type::Numeric {
                    return Err(ExecError::NodeInputType("ml_selector"));
                }
                let generated = model.generator.generate(stream.bytes(), stream.width());
                model
                    .predict(&generated)
                    .map_err(|_| ExecError::Parameter("ML selector prediction failed"))?
            };
            let successor = *successors
                .get(selected)
                .ok_or(ExecError::Parameter("Selected successor is out of bounds"))?;
            cx.set_destination(&[input], successor, None)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;

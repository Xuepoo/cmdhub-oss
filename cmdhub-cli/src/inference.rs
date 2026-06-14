use anyhow::Result;
use std::path::Path;
use tract_onnx::prelude::tract_ndarray::Array2;
use tract_onnx::prelude::*;

pub struct EmbeddingModel {
    plan: SimplePlan<TypedFact, Box<dyn TypedOp>, Graph<TypedFact, Box<dyn TypedOp>>>,
}

impl EmbeddingModel {
    pub fn load(path: &Path) -> Result<Self> {
        let model = tract_onnx::onnx()
            .model_for_path(path)?
            .into_optimized()?
            .into_runnable()?;
        Ok(Self { plan: model })
    }

    pub fn generate_embedding(&self, token_ids: &[i64], mask: &[i64]) -> Result<Vec<f32>> {
        let input_ids_array = Array2::from_shape_vec((1, 512), token_ids.to_vec())
            .map_err(|e| anyhow::anyhow!("Failed to create input_ids array: {}", e))?;
        let input_ids: Tensor = input_ids_array.into();

        let attention_mask_array = Array2::from_shape_vec((1, 512), mask.to_vec())
            .map_err(|e| anyhow::anyhow!("Failed to create attention_mask array: {}", e))?;
        let attention_mask: Tensor = attention_mask_array.into();

        let input_count = self.plan.model().inputs.len();
        let results = if input_count == 3 {
            let token_type_ids_array = Array2::from_shape_vec((1, 512), vec![0i64; 512])
                .map_err(|e| anyhow::anyhow!("Failed to create token_type_ids array: {}", e))?;
            let token_type_ids: Tensor = token_type_ids_array.into();
            self.plan.run(tvec![
                input_ids.into(),
                attention_mask.into(),
                token_type_ids.into()
            ])?
        } else {
            self.plan
                .run(tvec![input_ids.into(), attention_mask.into()])?
        };

        let output_tensor = results[0].to_array_view::<f32>()?;
        let shape = output_tensor.shape();

        let mut raw_vec = vec![0.0f32; 384];

        if shape.len() == 3 {
            let seq_len = shape[1];
            let dim = shape[2];
            let target_dim = std::cmp::min(dim, 384);

            let mut valid_token_count = 0.0f32;
            for (t, &m) in mask.iter().enumerate() {
                if t < seq_len && m > 0 {
                    let weight = m as f32;
                    valid_token_count += weight;
                    for d in 0..target_dim {
                        raw_vec[d] += output_tensor[[0, t, d]] * weight;
                    }
                }
            }

            if valid_token_count > 0.0 {
                for val in raw_vec.iter_mut().take(target_dim) {
                    *val /= valid_token_count;
                }
            }
        } else if shape.len() == 2 {
            let dim = shape[1];
            let target_dim = std::cmp::min(dim, 384);
            for d in 0..target_dim {
                raw_vec[d] = output_tensor[[0, d]];
            }
        } else {
            anyhow::bail!("Unexpected model output shape: {:?}", shape);
        }

        // L2 normalization
        let norm = raw_vec.iter().map(|&x| x * x).sum::<f32>().sqrt();
        let normalized_vec = if norm > 0.0 {
            raw_vec.into_iter().map(|x| x / norm).collect()
        } else {
            raw_vec
        };

        Ok(normalized_vec)
    }
}

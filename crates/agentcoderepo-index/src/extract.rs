//! LLM-based extraction of AgentCodeRepo type signatures from source code.

use anyhow::{Context, Result};
use agentcoderepo_llm::{LlmClient, Message};
use agentcoderepo_types::FunctionSig;
use agentcoderepo_types::parse::parse_ty;

/// The system prompt instructing the LLM to extract function signatures
/// in AgentCodeRepo's universal type syntax.
const EXTRACTION_SYSTEM_PROMPT: &str = r#"You are a type extraction engine for AgentCodeRepo, a universal code index.

Given source code, extract all exported/public functions, types, and values.
Express each one's type in AgentCodeRepo's universal type syntax.

## AgentCodeRepo Type Syntax

Primitives: Int, Float, String, Bool, Bytes, Unit, Never
Functions: `param -> ret` (pure), `param ->{Effect1, Effect2} ret` (effectful)
Generics: `forall a. a -> a`
Constraints: `forall a. Ord a => List a -> List a`
Higher-kinded: `forall (f : Type -> Type). Functor f => (a -> b) -> f a -> f b`
Records: `{ name: String, age: Int }`
Variants: `< Ok: a | Err: e >`
Tuples: `(Int, String)`
Type application: `List a`, `Map k v`, `Result e a`

## Effects

IO       - filesystem, network, system calls
Async    - requires async runtime
Fail e   - can fail with error type e
State s  - reads/modifies mutable state of type s
Rand     - uses randomness
Alloc    - heap allocation

## Output Format

Respond with a JSON array. Each element is an object with:
- "name": the function/value name
- "type": the AgentCodeRepo type signature as a string
- "description": a brief natural-language description

Example:
```json
[
  {
    "name": "sort",
    "type": "forall a. Ord a => List a -> List a",
    "description": "Sort a list using the natural ordering of its elements"
  },
  {
    "name": "http_get",
    "type": "String ->{IO, Fail HttpError} Response",
    "description": "Perform an HTTP GET request to the given URL"
  }
]
```

Only include public/exported items. Respond with ONLY the JSON array, no other text."#;

/// Extract function signatures from a source file using the LLM.
///
/// Returns a list of function signatures with types expressed as canonical
/// AgentCodeRepo type strings (not yet parsed into the Ty AST).
pub async fn extract_signatures(
    llm: &dyn LlmClient,
    file_path: &str,
    source: &str,
) -> Result<Vec<FunctionSig>> {
    let user_message = format!(
        "Extract all exported types, functions, and values from this file.\n\
         File: {file_path}\n\n\
         ```\n{source}\n```"
    );

    let response = llm
        .complete(&[
            Message {
                role: "system".to_string(),
                content: EXTRACTION_SYSTEM_PROMPT.to_string(),
            },
            Message {
                role: "user".to_string(),
                content: user_message,
            },
        ])
        .await
        .context("LLM completion failed")?;

    parse_extraction_response(&response.content)
}

/// Parse the LLM's JSON response into FunctionSig structs.
///
/// Attempts to parse each type string into a `Ty` AST using the AgentCodeRepo
/// type parser. Falls back to `Ty::Named(raw_string)` if parsing fails
/// (LLM output can be imperfect).
fn parse_extraction_response(response: &str) -> Result<Vec<FunctionSig>> {
    // Strip markdown code fences if present
    let json_str = response
        .trim()
        .strip_prefix("```json")
        .or_else(|| response.trim().strip_prefix("```"))
        .unwrap_or(response.trim());
    let json_str = json_str
        .strip_suffix("```")
        .unwrap_or(json_str)
        .trim();

    let items: Vec<RawExtraction> =
        serde_json::from_str(json_str).context("failed to parse LLM extraction response")?;

    Ok(items
        .into_iter()
        .map(|item| {
            let ty = match parse_ty(&item.ty) {
                Ok(parsed) => parsed,
                Err(e) => {
                    tracing::debug!(
                        name = %item.name,
                        raw_type = %item.ty,
                        error = %e,
                        "type parse failed, storing as Named"
                    );
                    agentcoderepo_types::Ty::Named(item.ty)
                }
            };
            FunctionSig {
                name: item.name,
                ty,
                description: item.description,
            }
        })
        .collect())
}

#[derive(serde::Deserialize)]
struct RawExtraction {
    name: String,
    #[serde(rename = "type")]
    ty: String,
    description: String,
}

// ---------------------------------------------------------------------------
// Logic-breaking change assessment
// ---------------------------------------------------------------------------

const LOGIC_ASSESSMENT_PROMPT: &str = r#"You are a backward-compatibility assessor for AgentCodeRepo, a universal code index.

Given the old and new versions of a function's source code, determine whether the behavioral change is BREAKING for existing callers.

A change is BREAKING if:
- The function returns different results for the same inputs
- Error handling changed (e.g., previously never failed, now can fail)
- Side effects changed meaningfully (e.g., now writes to disk when it didn't before)
- The function's contract/invariants changed

A change is NOT BREAKING if:
- Only performance changed (faster/slower but same results)
- Internal refactoring with identical behavior
- Comments or formatting changed
- Logging or debug output changed

Respond with ONLY a JSON object:
{"breaking": true, "reason": "brief explanation"} or {"breaking": false}"#;

/// Ask the LLM whether a function's logic change is breaking.
///
/// Returns `Some(reason)` if the LLM considers the change breaking,
/// `None` if it's safe or if assessment fails.
pub async fn assess_logic_breaking(
    llm: &dyn LlmClient,
    function_name: &str,
    old_source: &str,
    new_source: &str,
) -> Option<String> {
    let user_message = format!(
        "Function: {function_name}\n\n\
         OLD SOURCE:\n```\n{old_source}\n```\n\n\
         NEW SOURCE:\n```\n{new_source}\n```"
    );

    let response = match llm
        .complete(&[
            Message {
                role: "system".to_string(),
                content: LOGIC_ASSESSMENT_PROMPT.to_string(),
            },
            Message {
                role: "user".to_string(),
                content: user_message,
            },
        ])
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(function = %function_name, error = %e, "logic assessment LLM call failed");
            return None;
        }
    };

    parse_assessment_response(&response.content)
}

#[derive(serde::Deserialize)]
struct AssessmentResponse {
    breaking: bool,
    reason: Option<String>,
}

fn parse_assessment_response(response: &str) -> Option<String> {
    let json_str = response
        .trim()
        .strip_prefix("```json")
        .or_else(|| response.trim().strip_prefix("```"))
        .unwrap_or(response.trim());
    let json_str = json_str
        .strip_suffix("```")
        .unwrap_or(json_str)
        .trim();

    let assessment: AssessmentResponse = match serde_json::from_str(json_str) {
        Ok(a) => a,
        Err(e) => {
            tracing::warn!(error = %e, "failed to parse logic assessment response");
            return None;
        }
    };

    if assessment.breaking {
        Some(assessment.reason.unwrap_or_else(|| "logic change detected".to_string()))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_clean_json() {
        let json = r#"[
            {
                "name": "sort",
                "type": "forall a. Ord a => List a -> List a",
                "description": "Sort a list"
            }
        ]"#;
        let sigs = parse_extraction_response(json).unwrap();
        assert_eq!(sigs.len(), 1);
        assert_eq!(sigs[0].name, "sort");
        assert_eq!(sigs[0].description, "Sort a list");
        // Type should be parsed into a real Ty AST
        assert_eq!(sigs[0].ty.to_string(), "forall a. Ord a => List a -> List a");
        assert!(matches!(sigs[0].ty, agentcoderepo_types::Ty::Forall { .. }));
    }

    #[test]
    fn unparseable_type_falls_back_to_named() {
        let json = r#"[{
            "name": "weird",
            "type": "something the LLM made up ???",
            "description": "unparseable"
        }]"#;
        let sigs = parse_extraction_response(json).unwrap();
        assert_eq!(sigs.len(), 1);
        assert!(matches!(&sigs[0].ty, agentcoderepo_types::Ty::Named(s) if s.contains("???")));
    }

    #[test]
    fn parse_json_with_code_fences() {
        let json = "```json\n[{\"name\": \"f\", \"type\": \"Int -> Int\", \"description\": \"d\"}]\n```";
        let sigs = parse_extraction_response(json).unwrap();
        assert_eq!(sigs.len(), 1);
        assert_eq!(sigs[0].name, "f");
    }

    #[test]
    fn parse_empty_array() {
        let sigs = parse_extraction_response("[]").unwrap();
        assert!(sigs.is_empty());
    }
}

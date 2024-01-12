use super::{Activation, PyPiRequirement, SystemRequirements, Target, TargetSelector};
use crate::project::manifest::channel::{PrioritizedChannel, TomlPrioritizedChannelStrOrMap};
use crate::project::manifest::target::Targets;
use crate::project::SpecType;
use crate::task::Task;
use crate::utils::spanned::PixiSpanned;
use indexmap::IndexMap;
use itertools::Either;
use rattler_conda_types::{NamelessMatchSpec, PackageName, Platform};
use serde::de::Error;
use serde::{Deserialize, Deserializer};
use serde_with::{serde_as, DisplayFromStr, PickFirst};
use std::borrow::Cow;
use std::collections::HashMap;

/// The name of a feature. This is either a string or default for the default feature.
#[derive(Clone, Debug, Eq, PartialEq, PartialOrd, Ord, Hash)]
pub enum FeatureName {
    Default,
    Named(String),
}

impl<'de> Deserialize<'de> for FeatureName {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        match String::deserialize(deserializer)?.as_str() {
            "default" => Err(D::Error::custom(
                "The name 'default' is reserved for the default feature",
            )),
            name => Ok(FeatureName::Named(name.to_string())),
        }
    }
}

impl FeatureName {
    /// Returns the name of the feature or `None` if this is the default feature.
    pub fn name(&self) -> Option<&str> {
        match self {
            FeatureName::Default => None,
            FeatureName::Named(name) => Some(name),
        }
    }
}

/// A feature describes a set of functionalities. It allows us to group functionality and its
/// dependencies together.
///
/// Individual features cannot be used directly, instead they are grouped together into
/// environments. Environments are then locked and installed.
#[derive(Debug, Clone)]
pub struct Feature {
    /// The name of the feature or `None` if the feature is the default feature.
    pub name: FeatureName,

    /// The platforms this feature is available on.
    ///
    /// This value is `None` if this feature does not specify any platforms and the default
    /// platforms from the project should be used.
    pub platforms: Option<PixiSpanned<Vec<Platform>>>,

    /// Channels specific to this feature.
    ///
    /// This value is `None` if this feature does not specify any channels and the default
    /// channels from the project should be used.
    pub channels: Option<Vec<PrioritizedChannel>>,

    /// Additional system requirements
    pub system_requirements: SystemRequirements,

    /// Target specific configuration.
    pub targets: Targets,
}

impl Feature {
    /// Returns the dependencies of the feature for a given `spec_type` and `platform`.
    ///
    /// This function returns a [`Cow`]. If the dependencies are not combined or overwritten by
    /// multiple targets than this function returns a reference to the internal dependencies.
    ///
    /// Returns `None` if this feature does not define any target that has any of the requested
    /// dependencies.
    pub fn dependencies(
        &self,
        spec_type: Option<SpecType>,
        platform: Option<Platform>,
    ) -> Option<Cow<'_, IndexMap<PackageName, NamelessMatchSpec>>> {
        self.targets
            .resolve(platform)
            // Get the targets in reverse order, from least specific to most specific.
            // This is required because the extend function will overwrite existing keys.
            .rev()
            .filter_map(|t| t.dependencies(spec_type))
            .filter(|deps| !deps.is_empty())
            .fold(None, |acc, deps| match acc {
                None => Some(deps),
                Some(mut acc) => {
                    let deps_iter = match deps {
                        Cow::Borrowed(deps) => Either::Left(
                            deps.iter().map(|(name, spec)| (name.clone(), spec.clone())),
                        ),
                        Cow::Owned(deps) => Either::Right(deps.into_iter()),
                    };

                    acc.to_mut().extend(deps_iter);
                    Some(acc)
                }
            })
    }

    /// Returns the PyPi dependencies of the feature for a given `platform`.
    ///
    /// This function returns a [`Cow`]. If the dependencies are not combined or overwritten by
    /// multiple targets than this function returns a reference to the internal dependencies.
    ///
    /// Returns `None` if this feature does not define any target that has any of the requested
    /// dependencies.
    pub fn pypi_dependencies(
        &self,
        platform: Option<Platform>,
    ) -> Option<Cow<'_, IndexMap<rip::types::PackageName, PyPiRequirement>>> {
        self.targets
            .resolve(platform)
            // Get the targets in reverse order, from least specific to most specific.
            // This is required because the extend function will overwrite existing keys.
            .rev()
            .filter_map(|t| t.pypi_dependencies.as_ref())
            .filter(|deps| !deps.is_empty())
            .fold(None, |acc, deps| match acc {
                None => Some(Cow::Borrowed(deps)),
                Some(mut acc) => {
                    acc.to_mut().extend(
                        deps.into_iter()
                            .map(|(name, spec)| (name.clone(), spec.clone())),
                    );
                    Some(acc)
                }
            })
    }
}

impl<'de> Deserialize<'de> for Feature {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[serde_as]
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields, rename_all = "kebab-case")]
        struct FeatureInner {
            #[serde(default)]
            platforms: Option<PixiSpanned<Vec<Platform>>>,
            #[serde(default)]
            channels: Option<Vec<TomlPrioritizedChannelStrOrMap>>,
            #[serde(default)]
            system_requirements: SystemRequirements,
            #[serde(default)]
            target: IndexMap<PixiSpanned<TargetSelector>, Target>,

            #[serde(default)]
            #[serde_as(as = "IndexMap<_, PickFirst<(DisplayFromStr, _)>>")]
            dependencies: IndexMap<PackageName, NamelessMatchSpec>,

            #[serde(default)]
            #[serde_as(as = "Option<IndexMap<_, PickFirst<(DisplayFromStr, _)>>>")]
            host_dependencies: Option<IndexMap<PackageName, NamelessMatchSpec>>,

            #[serde(default)]
            #[serde_as(as = "Option<IndexMap<_, PickFirst<(DisplayFromStr, _)>>>")]
            build_dependencies: Option<IndexMap<PackageName, NamelessMatchSpec>>,

            #[serde(default)]
            pypi_dependencies: Option<IndexMap<rip::types::PackageName, PyPiRequirement>>,

            /// Additional information to activate an environment.
            #[serde(default)]
            activation: Option<Activation>,

            /// Target specific tasks to run in the environment
            #[serde(default)]
            tasks: HashMap<String, Task>,
        }

        let inner = FeatureInner::deserialize(deserializer)?;

        let mut dependencies = HashMap::from_iter([(SpecType::Run, inner.dependencies)]);
        if let Some(host_deps) = inner.host_dependencies {
            dependencies.insert(SpecType::Host, host_deps);
        }
        if let Some(build_deps) = inner.build_dependencies {
            dependencies.insert(SpecType::Build, build_deps);
        }

        let default_target = Target {
            dependencies,
            pypi_dependencies: inner.pypi_dependencies,
            activation: inner.activation,
            tasks: inner.tasks,
        };

        Ok(Feature {
            name: FeatureName::Default,
            platforms: inner.platforms,
            channels: inner.channels.map(|channels| {
                channels
                    .into_iter()
                    .map(|channel| channel.into_prioritized_channel())
                    .collect()
            }),
            system_requirements: inner.system_requirements,
            targets: Targets::from_default_and_user_defined(default_target, inner.target),
        })
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::project::manifest::Manifest;
    use assert_matches::assert_matches;
    use std::path::Path;

    #[test]
    fn test_dependencies_borrowed() {
        let manifest = Manifest::from_str(
            Path::new(""),
            r#"
        [project]
        name = "foo"
        platforms = ["linux-64", "osx-64", "win-64"]
        channels = []

        [dependencies]
        foo = "1.0"

        [host-dependencies]
        foo = "2.0"

        [feature.bla.dependencies]
        foo = "2.0"

        [feature.bla.host-dependencies]
        # empty on purpose
        "#,
        )
        .unwrap();

        assert_matches!(
            manifest
                .default_feature()
                .dependencies(Some(SpecType::Host), None)
                .unwrap(),
            Cow::Borrowed(_),
            "[host-dependencies] should be borrowed"
        );

        assert_matches!(
            manifest
                .default_feature()
                .dependencies(Some(SpecType::Run), None)
                .unwrap(),
            Cow::Borrowed(_),
            "[dependencies] should be borrowed"
        );

        assert_matches!(
            manifest.default_feature().dependencies(None, None).unwrap(),
            Cow::Owned(_),
            "combined dependencies should be owned"
        );

        let bla_feature = manifest
            .parsed
            .features
            .get(&FeatureName::Named(String::from("bla")))
            .unwrap();
        assert_matches!(
            bla_feature.dependencies(Some(SpecType::Run), None).unwrap(),
            Cow::Borrowed(_),
            "[feature.bla.dependencies] should be borrowed"
        );

        assert_matches!(
            bla_feature.dependencies(None, None).unwrap(),
            Cow::Borrowed(_),
            "[feature.bla] combined dependencies should also be borrowed"
        );
    }
}
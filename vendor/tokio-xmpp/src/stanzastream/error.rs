// Copyright (c) 2019 Emmanuel Gil Peyrot <linkmauve@linkmauve.fr>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use xso::{AsXml, FromXml};

static NS: &str = "https://xmlns.xmpp.rs/stream-errors";

/// Represents a parse error.
///
/// Details are found in the `<text/>`.
#[derive(FromXml, AsXml, Debug)]
#[xml(namespace = NS, name = "parse-error")]
pub struct ParseError;

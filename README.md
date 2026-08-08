# Package Version Server

## Overview

Package Version Server is a language server that enhances your development experience by providing versions of packages directly when hovering over keys in `package.json` files.

## Features

- Displays the version of a package upon hovering over its key in `package.json`.
- Seamless integration with popular code editors.
- Lightweight and easy to configure.


## Configuration

The server reads the user's npm configuration when it starts. It uses the file specified by `NPM_CONFIG_USERCONFIG` (or `npm_config_userconfig`), falling back to `~/.npmrc`.

Supported settings include:

- `registry` for unscoped packages
- `@scope:registry` for scoped packages
- Registry-specific `:_authToken` bearer tokens
- `strict-ssl` and `strictssl`
- Environment-variable references such as `${NPM_TOKEN}`

For example:

```ini
@internal:registry=https://artifactory.example.com/artifactory/api/npm/npm_prod_vir/
@internal-platform:registry=https://artifactory.example.com/api/npm/npm_prod_vir/
//artifactory.example.com/artifactory/api/npm/npm_prod_vir/:_authToken=${NPM_TOKEN}
strict-ssl=false
```

Authentication tokens are only sent to registry URLs matching the token's host and path. Restart the server after changing the npm configuration.

## Usage

Open a `package.json` file in your code editor, and hover over any dependency key to see the package version.

## Contributing

Feel free to submit issues or pull requests. Contributions are welcome!

New releases are published automatically whenever a new version tag is added on main branch.

## License

This project is licensed under the MIT License.

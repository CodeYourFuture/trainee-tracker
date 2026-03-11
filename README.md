# trainee-tracker

A website for tracking trainee progress, which also contains a grab-bag of tools which re-use code and auth.

We hope to replace all of this functionality with a well built system, but for now, it glues together different data sources conveniently.

## Local development

To develop locally, you need a few pieces, which you can configure in a `.env` file:

### Local config

You probably want to set exactly this: `CYF_TRAINEE_TRACKER_PUBLIC_BASE_URL=http://localhost:3000`

### GitHub OAuth app

Set up an OAuth application in https://github.com/settings/developers and set the following env vars:

* `CYF_TRAINEE_TRACKER_GITHUB_CLIENT_ID`
* `CYF_TRAINEE_TRACKER_GITHUB_CLIENT_SECRET`

### Google OAuth app

Set up an OAuth application in a project in https://console.cloud.google.com/auth/clients/create and set the following env vars:

* `CYF_TRAINEE_TRACKER_GOOGLE_APIS_CLIENT_ID`
* `CYF_TRAINEE_TRACKER_GOOGLE_APIS_CLIENT_SECRET`

### Slack (optional)

If you want Slack integration (you probably don't), make a Slack App and set `CYF_TRAINEE_TRACKER_SLACK_CLIENT_SECRET=`.

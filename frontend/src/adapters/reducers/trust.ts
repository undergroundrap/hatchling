import { defaultNetworkPermissions } from '../settings-fixtures.js';
import type { AppState, Command, CommandResult } from '../../protocol/models.js';
import { canApprove, canSubmit, maskCard } from '../../protocol/rules.js';
import { fail, nextId, ok, say } from './shared.js';
export function reduceTrust(state: AppState, command: Command): CommandResult | null {
    switch (command.type) {
        case 'permission.website': {
            const item = state.websitePermissions.find(entry => entry.host === command.host);
            if (!item)
                return fail('That website grant is no longer available.');
            item.policy = command.policy;
            return ok(`Website permission set to ${command.policy} in the local demo.`);
        }
        case 'skill.enable': {
            const skill = state.skills.find(entry => entry.name === command.name);
            if (!skill)
                return fail('This sample skill is no longer here.');
            if (skill.digest !== command.digest)
                return fail('This sample skill changed since you were shown it, so nothing was enabled. Review it again.');
            skill.state = 'enabled';
            return ok('Enabled in the local demo. No skill runs here.');
        }
        case 'skill.disable': {
            const skill = state.skills.find(entry => entry.name === command.name);
            if (!skill)
                return fail('This sample skill is no longer here.');
            skill.state = 'waiting';
            return ok('Disabled in the local demo.');
        }
        case 'permission.network': {
            const item = state.networkPermissions.find(entry => entry.id === command.id);
            if (!item)
                return fail('That network setting is not available.');
            item.enabled = command.enabled;
            return ok('Network preference updated locally. No network connection was made.');
        }
        case 'purchase.approve': {
            const purchase = state.purchases.find((item) => item.id === command.id);
            if (!purchase)
                return fail('This purchase review is no longer available.');
            const error = canApprove(purchase, command.revision, command.total, state.spendingLimit - state.spentToday);
            if (error)
                return fail(error);
            purchase.state = 'approved';
            purchase.reviewState = 'approved';
            purchase.approvedTotal = command.total;
            purchase.approvedRevision = command.revision;
            state.turn = 'idle';
            return ok('Approved this exact quote. The order has not yet been submitted.');
        }
        case 'purchase.submit': {
            const purchase = state.purchases.find((item) => item.id === command.id);
            if (!purchase)
                return fail('This purchase is no longer available.');
            const error = canSubmit(purchase);
            if (error)
                return fail(error);
            const available = state.spendingLimit - state.spentToday;
            if (purchase.total > available)
                return fail(`Your remaining limit is now $${available.toFixed(2)}. This $${purchase.total.toFixed(2)} order was not submitted.`);
            purchase.state = 'submitted';
            purchase.ordersSubmitted += 1;
            state.spentToday = Math.round((state.spentToday + purchase.total) * 100) / 100;
            const task = state.browserTasks.find((item) => item.id === purchase.taskId);
            if (task) {
                task.state = 'running';
                task.step = 'Submitting the single approved sample order';
                task.statusTitle = 'Submitting order';
            }
            return ok('Submitting the sample order once. No real payment is made.');
        }
        case 'purchase.decline': {
            const purchase = state.purchases.find((item) => item.id === command.id);
            if (!purchase || !['proposed', 'approved'].includes(purchase.state))
                return fail('A submitted order cannot be declined here. Take over to review the merchant’s cancellation options.');
            purchase.state = 'denied';
            purchase.reviewState = 'declined';
            state.turn = 'idle';
            const task = state.browserTasks.find((item) => item.id === purchase.taskId);
            if (task)
                task.state = 'cancelled';
            return ok('Declined. The task stopped and nothing was submitted.');
        }
        case 'purchase.method': {
            const purchase = state.purchases.find((item) => item.id === command.id);
            const method = state.providers.flatMap((provider) => provider.methods).find((item) => item.id === command.methodId);
            if (!purchase || !method || !['proposed', 'approved'].includes(purchase.state))
                return fail('The payment method cannot be changed after submission.');
            purchase.methodId = method.id;
            purchase.fees = method.fee;
            purchase.total = Math.round((purchase.subtotal + purchase.tax + purchase.shippingCost + purchase.fees) * 100) / 100;
            purchase.revision++;
            purchase.state = 'proposed';
            purchase.reviewState = 'changed';
            purchase.approvedTotal = null;
            purchase.approvedRevision = null;
            return ok(`${maskCard(method)} selected. Review and approve the updated quote.`);
        }
        case 'purchase.requote': {
            const purchase = state.purchases.find((item) => item.id === command.id);
            if (!purchase || !['proposed', 'approved'].includes(purchase.state) || command.total < 0)
                return fail('This quote can no longer be changed.');
            purchase.fees = Math.round((purchase.fees + command.total - purchase.total) * 100) / 100;
            purchase.total = command.total;
            purchase.revision++;
            purchase.state = 'proposed';
            purchase.reviewState = 'changed';
            purchase.approvedTotal = null;
            purchase.approvedRevision = null;
            return ok('The total changed. The old approval is invalid; nothing was submitted.');
        }
        case 'connector.begin':
        case 'connector.finish':
        case 'connector.disconnect': {
            const connector = state.connectors.find((item) => item.id === command.id);
            if (!connector)
                return fail('Connection not found.');
            if (command.type === 'connector.begin') {
                connector.state = 'pending';
                return ok('A secure connection handoff is ready. This is a local preview, not a live sign-in.');
            }
            if (command.type === 'connector.finish') {
                if (connector.state !== 'pending')
                    return fail('Start the secure connection handoff first.');
                connector.state = 'connected';
                return ok(`${connector.name} is connected in the scripted workspace.`);
            }
            connector.state = 'available';
            return ok(`${connector.name} disconnected in this demo. Its grant has been removed.`);
        }
        case 'channel.begin':
        case 'channel.finish':
        case 'channel.check':
        case 'channel.disconnect':
        case 'channel.restart': {
            const channel = state.channels.find((item) => item.id === command.id);
            if (!channel)
                return fail('Channel not found.');
            if (command.type === 'channel.begin') {
                if (channel.state === 'checking')
                    return fail('The connection is being checked. Please re-check its status instead.');
                if (channel.state === 'unavailable')
                    return fail('This channel is temporarily unavailable. Try again later.');
                if (channel.state === 'linked')
                    return fail('This channel is already linked.');
                channel.state = 'link_pending';
                channel.lastEvent = 'Waiting for the existing secure connection handoff.';
                return ok('Continue through the secure connection handoff.');
            }
            if (command.type === 'channel.finish') {
                if (channel.state !== 'link_pending')
                    return fail('Start the secure connection handoff first.');
                channel.state = 'linked';
                channel.polling = true;
                channel.lastEvent = 'Listening for your messages';
                if (!state.chats.some((chat) => chat.id === channel.chatId))
                    state.chats.push({ id: channel.chatId, title: channel.name, channelId: channel.id, pinned: false, archived: false, unread: true });
                if (!channel.greetingSent) {
                    say(state, 'Hi, I’m here. This conversation is now linked to your personal agent.', channel.chatId);
                    channel.greetingSent = true;
                }
                return ok('Linked in the demo. The one-time greeting is in its dedicated conversation.');
            }
            if (command.type === 'channel.check') {
                if (channel.state === 'unavailable')
                    return fail('This channel is still temporarily unavailable. Try again later.');
                if (channel.state === 'checking') {
                    channel.state = 'linked';
                    channel.polling = true;
                    channel.lastEvent = 'Connection checked. Listening for your messages.';
                }
                return ok(channel.lastEvent);
            }
            if (command.type === 'channel.restart') {
                if (channel.state !== 'linked')
                    return fail('Link the channel before restarting delivery.');
                channel.polling = true;
                channel.lastEvent = 'Listening for your messages';
                return ok('The sample listening worker restarted.');
            }
            channel.polling = false;
            channel.state = 'unlinked';
            channel.lastEvent = 'Disconnected. Listening stopped; a goodbye was delivered.';
            say(state, 'Goodbye for now. This channel is disconnected, and I have stopped listening here.', channel.chatId);
            return ok('Disconnected. Polling stopped and a goodbye was added to the channel.');
        }
        case 'wallet.connect':
        case 'wallet.reauth':
        case 'wallet.add-method': {
            const provider = state.providers.find((item) => item.id === command.id);
            if (!provider)
                return fail('Wallet provider not found.');
            if (provider.comingSoon)
                return fail('This payment provider is not available.');
            provider.state = 'connected';
            if (command.type === 'wallet.add-method' && provider.methods.length === 0)
                provider.methods.push({ id: nextId(state, 'method'), brand: 'Visa', last4: '1881', provider: provider.name, fee: provider.id === 'virtual' ? 1 : 0 });
            return ok(command.type === 'wallet.add-method' ? 'A masked sample method was added. No card details were collected.' : 'The provider is connected in the demo. Add a sample method through the secure-page preview.');
        }
        case 'permission.set': {
            const permission = state.permissions.find((item) => item.id === command.id);
            if (!permission)
                return fail('Permission not found.');
            if (permission.id === 'payments' && command.mode !== 'always-ask')
                return fail('Purchases always require an exact-quote approval.');
            permission.mode = command.mode;
            return ok('Permission default updated. Sensitive actions still require explicit approval.');
        }
        case 'permission.reset': {
            state.networkPermissions = defaultNetworkPermissions().map(item => ({ ...item, enabled: false }));
            for (const item of state.websitePermissions)
                item.policy = 'ask';
            for (const permission of state.permissions)
                permission.mode = ['payments', 'scheduled'].includes(permission.id) ? 'always-ask' : 'ask-for-some';
            state.settings.captchaStanding = false;
            return ok('Defaults restored. Active challenge permissions were removed.');
        }
        case 'browser.takeover':
        case 'browser.resume':
        case 'browser.grant':
        case 'browser.solve':
        case 'browser.cancel':
        case 'browser.start': {
            const task = state.browserTasks.find((item) => item.id === command.id);
            if (!task)
                return fail('Browser task not found.');
            if (command.type === 'browser.cancel') {
                task.state = 'cancelled';
                task.controller = 'user';
                state.turn = 'idle';
                return ok('The browser task stopped. It will not automatically resume.');
            }
            if (command.type === 'browser.takeover') {
                task.controller = 'user';
                return ok(`You have control of the sample session for ${task.site}.`);
            }
            if (command.type === 'browser.start') {
                if (!['assigned', 'queued'].includes(task.state))
                    return fail('Only an assigned or queued task can be started. This task will not restart.');
                if (state.browserTasks.some((other) => other.id !== task.id && ['running', 'parked'].includes(other.state)))
                    return fail('Another task holds the browser slot. This task stays queued.');
                if (!state.settings.allowlist.includes(task.site))
                    return fail('This site is not on the navigation allowlist. Add it in Permissions first.');
                task.state = 'running';
                return ok('The scripted browser task is running.');
            }
            if (command.type === 'browser.solve') {
                if (task.controller !== 'user')
                    return fail('Take over before completing the site step yourself.');
                task.challenge = 'solved';
                task.lastConfirmedState = 'Fresh sample snapshot confirms the challenge was completed by the user.';
                task.blocker = '';
                return ok('The sample site step is complete. Hand control back when ready.');
            }
            if (command.type === 'browser.grant') {
                if (task.challenge === 'persistent' || task.attemptsRemaining <= 0) {
                    task.controller = 'user';
                    return ok('No more automated attempts. Take over to complete the challenge yourself.');
                }
                if (task.challenge !== 'waiting')
                    return fail('There is no pending challenge to approve.');
                task.granted.push('One-time permission for this challenge');
                task.attemptsRemaining--;
                task.challenge = 'solved';
                task.state = 'running';
                task.blocker = '';
                task.lastConfirmedState = 'The permitted sample challenge completed. Delivery options are visible.';
                state.turn = 'idle';
                return ok('Permission applied to this challenge only. No standing permission was created.');
            }
            if (task.challenge === 'waiting' || task.challenge === 'persistent')
                return fail('Complete the challenge in takeover before handing control back.');
            if (state.purchases.some((purchase) => purchase.taskId === task.id && purchase.state === 'unknown'))
                return fail('The payment outcome is unknown. Keep control and check the merchant; the agent will not retry.');
            if (['cancelled', 'completed', 'failed'].includes(task.state))
                return fail('This task has ended. It cannot automatically resume.');
            task.controller = 'agent';
            task.continuedFrom = task.id;
            task.state = 'running';
            task.lastConfirmedState = 'Fresh sample snapshot checked after takeover.';
            return ok('Control handed back after checking the fresh sample state.');
        }
        case 'device.pair': {
            if (!command.name.trim())
                return fail('Give the device a name.');
            state.devices.push({ id: nextId(state, 'device'), name: command.name.trim(), type: 'phone', reachable: true, capabilities: ['Show notification'], lastSeen: 'Paired in this demo', origin: false });
            return ok('The sample device is paired. No real device was contacted.');
        }
        case 'device.remove': {
            const device = state.devices.find((item) => item.id === command.id);
            if (!device || device.origin)
                return fail('The originating computer cannot be removed from this local demo.');
            state.devices = state.devices.filter((item) => item.id !== command.id);
            return ok('Device removed. Its capabilities are no longer available.');
        }
        case 'device.command': {
            const device = state.devices.find((item) => item.id === command.id);
            if (!device?.reachable)
                return fail('That device is not reachable. The action was not routed to a different device.');
            if (!device.capabilities.includes(command.capability))
                return fail('That capability is not advertised by this device.');
            state.notifications.unshift({ id: nextId(state, 'device-event'), title: `${device.name}: ${command.capability}`, text: 'The action ran on this explicitly selected sample device. Synced data was not posted into chat.', time: '9:41 am', read: false, urgent: false, evidenceId: null, offer: null, stage: 'delivered' });
            return ok(`Sample action completed on ${device.name}. No device switch occurred.`);
        }
        default: return null;
    }
}
export function settlePurchases(state: AppState): void {
    for (const purchase of state.purchases.filter((item) => item.state === 'submitted')) {
        purchase.state = purchase.outcome;
        const task = state.browserTasks.find((item) => item.id === purchase.taskId);
        if (task) {
            task.state = purchase.outcome === 'confirmed' ? 'completed' : 'parked';
            task.step = purchase.outcome === 'confirmed' ? 'Confirmed the single sample bag order' : 'Checking the submitted sample order status';
            task.statusTitle = purchase.outcome === 'confirmed' ? 'Confirmed order' : 'Checking order';
            task.lastConfirmedState = purchase.outcome === 'confirmed' ? 'Sample confirmation page verified; one order.' : 'Submission occurred; confirmation is not verified.';
            task.blocker = purchase.outcome === 'unknown' ? 'Payment outcome unknown. Never retry; check the merchant in takeover to avoid duplicate payment.' : '';
        }
        say(state, purchase.outcome === 'confirmed' ? 'The sample order is confirmed, and the confirmation page was checked. No real payment was made.' : purchase.outcome === 'unknown' ? 'The payment outcome is unknown, so I will not retry. Take over and check the merchant’s orders before doing anything else — another payment could create a duplicate.' : 'The merchant declined the payment. The task has stopped; take over to use the merchant’s secure page.');
    }
}

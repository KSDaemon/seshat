// CommonJS middleware — .js with require/module.exports
const { validateEmail } = require('./utils');
const logger = require('./logger');

function authMiddleware(req, res, next) {
    if (!req.headers.authorization) {
        return res.status(401).send('Unauthorized');
    }
    next();
}

async function rateLimiter(req, res, next) {
    // Rate limiting logic
    logger.info('Rate limiter check');
    next();
}

const errorHandler = (err, req, res, next) => {
    logger.error(err.message);
    res.status(500).send('Internal Server Error');
};

exports.authMiddleware = authMiddleware;
exports.rateLimiter = rateLimiter;
module.exports.errorHandler = errorHandler;
